use lopdf::{
    content::{Content, Operation},
    dictionary, Document, Object, Stream,
};
use ocr_domain::{IdempotencyKey, JobId, PageTask, ProductId, RequestDigest, TenantId, UploadId};
use ocr_service::{
    AcceptedPageSourceLoader, AcceptedSourceBytesReader, AcceptedSourceBytesReaderFuture,
    AcceptedSourceReadError, NativePageRecognizer, PageRecognitionError, PageRecognizer,
    ParserProcess,
};
use ocr_store::{CreateJob, PgJobStore, StoredAcceptedSource};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

struct LocalObjectReader {
    path: PathBuf,
    reads: AtomicUsize,
}
impl AcceptedSourceBytesReader for LocalObjectReader {
    fn read<'a>(
        &'a self,
        _product: &'a ProductId,
        _tenant: &'a TenantId,
        source: &'a StoredAcceptedSource,
    ) -> AcceptedSourceBytesReaderFuture<'a> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let bytes = tokio::fs::read(&self.path)
                .await
                .map_err(|_| AcceptedSourceReadError::Unavailable)?;
            let digest = format!(
                "sha256:{}",
                Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            if digest != source.digest
                || i64::try_from(bytes.len()).ok() != Some(source.content_length)
            {
                return Err(AcceptedSourceReadError::Invalid);
            }
            Ok(bytes)
        })
    }
}

fn pdf() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let font = document
        .add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = document.add_object(dictionary! {"Font"=>dictionary!{"F1"=>font}});
    let mut children = vec![];
    for text in ["Native invoice", "Second page"] {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![10.into(), 200.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let stream = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page=document.add_object(dictionary!{"Type"=>"Page","Parent"=>pages_id,"MediaBox"=>vec![0.into(),0.into(),240.into(),240.into()],"Resources"=>resources,"Contents"=>stream});
        children.push(Object::Reference(page));
    }
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>children,"Count"=>2}),
    );
    let catalog = document.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages_id});
    document.trailer.set("Root", catalog);
    let mut bytes = vec![];
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn parser() -> ParserProcess {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ocr-parser-sandbox");
    ParserProcess::new(binary, Duration::from_secs(30))
        .unwrap()
        .with_pdf_text_executable(if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/pdftotext".into()
        } else {
            "/usr/bin/pdftotext".into()
        })
        .unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and built ocr-parser-sandbox"]
async fn real_native_processing_survives_worker_restart_without_rereading_source() {
    let admin = sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_ADMIN_URL").unwrap())
        .await
        .unwrap();
    let store = PgJobStore::new(
        sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap(),
    );
    let tenant = TenantId::new("ten_NATIVE_REAL").unwrap();
    let product = ProductId::new("kora").unwrap();
    let job = JobId::new("job_NATIVE_REAL").unwrap();
    let upload = UploadId::new("upl_NATIVE_REAL").unwrap();
    sqlx::query("delete from ocr_outbox where job_id=$1")
        .bind(job.as_str())
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_jobs where job_id=$1")
        .bind(job.as_str())
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("delete from ocr_uploads where upload_id=$1")
        .bind(upload.as_str())
        .execute(&admin)
        .await
        .unwrap();
    let bytes = pdf();
    let digest = format!(
        "sha256:{}",
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    sqlx::query("insert into ocr_uploads (upload_id,tenant_id,product_id,idempotency_key,request_digest,object_bucket,object_name,expected_content_type,expected_content_length,expected_digest,status,expires_at,object_generation,verified_content_type,verified_content_length,verified_digest,uploaded_at,source_bucket,source_object_name,source_object_generation,source_digest,source_content_length,inspection_attempts,accepted_at,parser_page_count,parser_maximum_page_pixels,parser_total_page_pixels,parser_profile,parser_version,parser_page_geometries) values ($1,$2,'kora','native-real',$3,'dev-kora-ocr-quarantine',$4,'application/pdf',$5,$3,'accepted',now()+interval '10 minutes',1,'application/pdf',$5,$3,now(),'dev-kora-ocr-source',$6,2,$3,$5,1,now(),2,1000000,2000000,'strict-v1','native-test-v1',$7::jsonb)")
        .bind(upload.as_str()).bind(tenant.as_str()).bind(&digest).bind("products/kora/tenants/ten_NATIVE_REAL/upl_NATIVE_REAL").bind(bytes.len() as i64).bind("products/kora/tenants/ten_NATIVE_REAL/upl_NATIVE_REAL/accepted").bind(r#"[{"page":1,"width":1000,"height":1000},{"page":2,"width":1000,"height":1000}]"#).execute(&admin).await.unwrap();
    store
        .create(CreateJob {
            job_id: job.clone(),
            tenant_id: tenant.clone(),
            product_id: product.clone(),
            idempotency_key: IdempotencyKey::new("native-real").unwrap(),
            request_digest: RequestDigest::new(&digest).unwrap(),
            upload_id: upload,
            webhook_subscription_id: None,
            extraction: None,
        })
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.pdf");
    tokio::fs::write(&path, &bytes).await.unwrap();
    let reader = Arc::new(LocalObjectReader {
        path,
        reads: AtomicUsize::new(0),
    });
    let source = Arc::new(AcceptedPageSourceLoader::new(
        Arc::new(store.clone()),
        reader.clone(),
    ));
    let first = NativePageRecognizer::new(source.clone(), parser(), store.clone());
    let task = PageTask {
        page: 1,
        attempt: 1,
        activity_key: "native-real-1".to_owned(),
    };
    let page = first
        .recognize(&product, &tenant, &job, &task)
        .await
        .unwrap();
    assert_eq!(page.observations[0].text, "Native invoice");
    drop(first);
    let restarted = NativePageRecognizer::new(source, parser(), store.clone());
    let next = PageTask {
        page: 2,
        attempt: 1,
        activity_key: "native-real-2".to_owned(),
    };
    assert_eq!(
        restarted
            .recognize(&product, &tenant, &job, &next)
            .await
            .unwrap()
            .observations[0]
            .text,
        "Second page"
    );
    assert_eq!(reader.reads.load(Ordering::SeqCst), 1);
    assert!(matches!(
        restarted
            .recognize(
                &product,
                &TenantId::new("ten_OTHER_NATIVE_REAL").unwrap(),
                &job,
                &next
            )
            .await,
        Err(PageRecognitionError::Permanent)
    ));
    assert_eq!(reader.reads.load(Ordering::SeqCst), 1);
    store
        .clear_native_pages(&tenant, &product, &job)
        .await
        .unwrap();
}
