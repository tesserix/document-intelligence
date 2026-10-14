use crate::{set_scope, Error, PgJobStore, Result};
use futures_util::TryStreamExt;
use ocr_domain::{DocumentPage, JobId, ProductId, TenantId, MAXIMUM_PAGE_COUNT};
use sqlx::{Postgres, Row, Transaction};

const MAXIMUM_NATIVE_BYTES: usize = ocr_domain::MAXIMUM_DOCUMENT_RESULT_BYTES;

impl PgJobStore {
    pub async fn load_native_page(
        &self,
        tenant: &TenantId,
        product: &ProductId,
        job: &JobId,
        profile: &str,
        page: u32,
    ) -> Result<Option<DocumentPage>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant, product).await?;
        let row = sqlx::query("select p.page_data::text as page_data from ocr_native_pages p join ocr_native_documents n using (native_document_id,product_id,tenant_id) join ocr_jobs j on j.job_id=n.job_id and j.product_id=n.product_id and j.tenant_id=n.tenant_id join ocr_uploads u on u.upload_id=j.upload_id and u.product_id=j.product_id and u.tenant_id=j.tenant_id where n.job_id=$1 and n.product_id=$2 and n.tenant_id=$3 and n.processing_profile=$4 and n.source_digest=u.source_digest and p.page_number=$5")
            .bind(job.as_str()).bind(product.as_str()).bind(tenant.as_str()).bind(profile).bind(i32::try_from(page).map_err(|_| Error::InvalidStoredPageArtifact)?).fetch_optional(&mut *transaction).await?;
        transaction.commit().await?;
        row.map(|row| {
            let encoded: String = row.try_get("page_data")?;
            if encoded.len() > MAXIMUM_NATIVE_BYTES * 2 {
                return Err(Error::InvalidStoredPageArtifact);
            }
            let value: DocumentPage =
                serde_json::from_str(&encoded).map_err(|_| Error::InvalidStoredPageArtifact)?;
            if u32::from(value.page) != page {
                return Err(Error::InvalidStoredPageArtifact);
            }
            Ok(value)
        })
        .transpose()
    }

    pub async fn load_native_pages(
        &self,
        tenant: &TenantId,
        product: &ProductId,
        job: &JobId,
        profile: &str,
    ) -> Result<Option<Vec<DocumentPage>>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant, product).await?;
        let pages = load_pages(&mut transaction, tenant, product, job, profile, None).await?;
        transaction.commit().await?;
        Ok(pages)
    }

    pub async fn save_native_pages(
        &self,
        tenant: &TenantId,
        product: &ProductId,
        job: &JobId,
        profile: &str,
        source_digest: &str,
        pages: &[DocumentPage],
    ) -> Result<Vec<DocumentPage>> {
        if pages.is_empty()
            || pages.len() > MAXIMUM_PAGE_COUNT as usize
            || pages
                .iter()
                .enumerate()
                .any(|(i, p)| u32::from(p.page) != i as u32 + 1)
        {
            return Err(Error::InvalidStoredPageArtifact);
        }
        let encoded = serde_json::to_string(pages).map_err(|_| Error::InvalidStoredPageArtifact)?;
        if encoded.len() > MAXIMUM_NATIVE_BYTES {
            return Err(Error::InvalidStoredPageArtifact);
        }
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant, product).await?;
        let row = sqlx::query("insert into ocr_native_documents (job_id,product_id,tenant_id,processing_profile,source_digest,page_count,duration_ms) select j.job_id,j.product_id,j.tenant_id,$4,u.source_digest,$6,greatest(0,floor(extract(epoch from(statement_timestamp()-j.created_at))*1000))::bigint from ocr_jobs j join ocr_uploads u on u.upload_id=j.upload_id and u.product_id=j.product_id and u.tenant_id=j.tenant_id where j.job_id=$1 and j.product_id=$2 and j.tenant_id=$3 and u.status='accepted' and u.source_digest=$5 and u.parser_page_count=$6 on conflict (job_id,processing_profile,source_digest) do nothing returning native_document_id")
            .bind(job.as_str()).bind(product.as_str()).bind(tenant.as_str()).bind(profile).bind(source_digest).bind(pages.len() as i32).fetch_optional(&mut *transaction).await?;
        if let Some(row) = row {
            let id: i64 = row.try_get("native_document_id")?;
            sqlx::query("insert into ocr_native_pages (native_document_id,product_id,tenant_id,page_number,page_data) select $1,$2,$3,(page->>'page')::integer,page from jsonb_array_elements($4::jsonb) page")
                .bind(id).bind(product.as_str()).bind(tenant.as_str()).bind(&encoded).execute(&mut *transaction).await?;
        }
        let canonical = load_pages(
            &mut transaction,
            tenant,
            product,
            job,
            profile,
            Some(source_digest),
        )
        .await?
        .ok_or(Error::InvalidStoredPageArtifact)?;
        transaction.commit().await?;
        Ok(canonical)
    }

    pub async fn load_native_duration_ms(
        &self,
        tenant: &TenantId,
        product: &ProductId,
        job: &JobId,
        profile: &str,
    ) -> Result<Option<u64>> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant, product).await?;
        let value: Option<i64> = sqlx::query_scalar("select n.duration_ms from ocr_native_documents n join ocr_jobs j on j.job_id=n.job_id and j.product_id=n.product_id and j.tenant_id=n.tenant_id join ocr_uploads u on u.upload_id=j.upload_id and u.product_id=j.product_id and u.tenant_id=j.tenant_id where n.job_id=$1 and n.product_id=$2 and n.tenant_id=$3 and n.processing_profile=$4 and n.source_digest=u.source_digest")
            .bind(job.as_str()).bind(product.as_str()).bind(tenant.as_str()).bind(profile).fetch_optional(&mut *transaction).await?;
        transaction.commit().await?;
        value
            .map(|value| u64::try_from(value).map_err(|_| Error::InvalidStoredPageArtifact))
            .transpose()
    }

    pub async fn clear_native_pages(
        &self,
        tenant: &TenantId,
        product: &ProductId,
        job: &JobId,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        set_scope(&mut transaction, tenant, product).await?;
        sqlx::query(
            "delete from ocr_native_documents where job_id=$1 and product_id=$2 and tenant_id=$3",
        )
        .bind(job.as_str())
        .bind(product.as_str())
        .bind(tenant.as_str())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn load_pages(
    transaction: &mut Transaction<'_, Postgres>,
    tenant: &TenantId,
    product: &ProductId,
    job: &JobId,
    profile: &str,
    source_digest: Option<&str>,
) -> Result<Option<Vec<DocumentPage>>> {
    let mut rows = sqlx::query("select n.page_count,p.page_data::text as page_data,sum(octet_length(p.page_data::text)) over ()::bigint as total_bytes from ocr_native_pages p join ocr_native_documents n using (native_document_id,product_id,tenant_id) join ocr_jobs j on j.job_id=n.job_id and j.product_id=n.product_id and j.tenant_id=n.tenant_id join ocr_uploads u on u.upload_id=j.upload_id and u.product_id=j.product_id and u.tenant_id=j.tenant_id where n.job_id=$1 and n.product_id=$2 and n.tenant_id=$3 and n.processing_profile=$4 and n.source_digest=u.source_digest and ($5::text is null or n.source_digest=$5) order by p.page_number")
        .bind(job.as_str()).bind(product.as_str()).bind(tenant.as_str()).bind(profile).bind(source_digest).fetch(&mut **transaction);
    let mut pages = Vec::new();
    let mut count = 0_usize;
    while let Some(row) = rows.try_next().await? {
        count = usize::try_from(row.try_get::<i32, _>("page_count")?)
            .map_err(|_| Error::InvalidStoredPageArtifact)?;
        let total_bytes = usize::try_from(row.try_get::<i64, _>("total_bytes")?)
            .map_err(|_| Error::InvalidStoredPageArtifact)?;
        // JSONB adds whitespace; the stored representation is bounded separately.
        if count == 0
            || count > MAXIMUM_PAGE_COUNT as usize
            || pages.len() >= count
            || total_bytes > MAXIMUM_NATIVE_BYTES * 2
        {
            return Err(Error::InvalidStoredPageArtifact);
        }
        let encoded: String = row.try_get("page_data")?;
        let page: DocumentPage =
            serde_json::from_str(&encoded).map_err(|_| Error::InvalidStoredPageArtifact)?;
        if u32::from(page.page) != pages.len() as u32 + 1 {
            return Err(Error::InvalidStoredPageArtifact);
        }
        pages.push(page);
    }
    if pages.is_empty() {
        return Ok(None);
    }
    if pages.len() != count {
        return Err(Error::InvalidStoredPageArtifact);
    }
    Ok(Some(pages))
}
