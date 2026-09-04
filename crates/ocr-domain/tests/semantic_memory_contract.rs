use ocr_domain::{
    ChunkId, DocumentId, DocumentVersion, EmbeddingVersion, MemoryRecordId, MemoryRecordVersion,
    ObservationId, SemanticCollection, SemanticQueryScope, TenantId, VectorPointId,
    VectorPointMetadata, VectorPointMetadataInput,
};

#[test]
fn vector_point_identity_is_replay_stable_and_tenant_version_specific() {
    let tenant = TenantId::new("ten_ALPHA").unwrap();
    let record = MemoryRecordId::new("mem_INVOICE_TOTAL").unwrap();
    let record_version = MemoryRecordVersion::new(&format!("sha256:{}", "a".repeat(64))).unwrap();
    let embedding = EmbeddingVersion::new("emb_printed_en_1.0").unwrap();

    let first = VectorPointId::derive(&tenant, &record, &record_version, &embedding);
    let replay = VectorPointId::derive(&tenant, &record, &record_version, &embedding);
    let other_tenant = VectorPointId::derive(
        &TenantId::new("ten_BETA").unwrap(),
        &record,
        &record_version,
        &embedding,
    );
    let other_embedding = VectorPointId::derive(
        &tenant,
        &record,
        &record_version,
        &EmbeddingVersion::new("emb_printed_en_2.0").unwrap(),
    );
    let other_record_version = VectorPointId::derive(
        &tenant,
        &record,
        &MemoryRecordVersion::new(&format!("sha256:{}", "c".repeat(64))).unwrap(),
        &embedding,
    );

    assert_eq!(first, replay);
    assert_ne!(first, other_tenant);
    assert_ne!(first, other_embedding);
    assert_ne!(first, other_record_version);
    assert_eq!(
        uuid::Uuid::parse_str(first.as_str())
            .unwrap()
            .get_version_num(),
        8
    );
}

#[test]
fn semantic_query_scope_requires_tenant_versioned_collection_and_bounded_limit() {
    let scope = SemanticQueryScope::new(
        TenantId::new("ten_ALPHA").unwrap(),
        SemanticCollection::new(1, 3).unwrap(),
        25,
    )
    .unwrap();

    assert_eq!(scope.tenant_id().as_str(), "ten_ALPHA");
    assert_eq!(scope.collection().alias(), "ocr-memory-s1-e3");
    assert_eq!(scope.limit(), 25);
    assert!(SemanticCollection::new(0, 1).is_err());
    assert!(SemanticCollection::new(1, 0).is_err());
    assert!(EmbeddingVersion::new("emb_-invalid").is_err());
    assert!(SemanticQueryScope::new(
        TenantId::new("ten_ALPHA").unwrap(),
        SemanticCollection::new(1, 1).unwrap(),
        0,
    )
    .is_err());
    assert!(SemanticQueryScope::new(
        TenantId::new("ten_ALPHA").unwrap(),
        SemanticCollection::new(1, 1).unwrap(),
        101,
    )
    .is_err());
}

#[test]
fn vector_metadata_is_allowlisted_and_resolves_to_canonical_evidence() {
    let metadata = VectorPointMetadata::new(VectorPointMetadataInput {
        tenant_id: TenantId::new("ten_ALPHA").unwrap(),
        memory_record_id: MemoryRecordId::new("mem_INVOICE_TOTAL").unwrap(),
        memory_record_version: MemoryRecordVersion::new(&format!("sha256:{}", "a".repeat(64)))
            .unwrap(),
        document_id: DocumentId::new("doc_INVOICE").unwrap(),
        document_version: DocumentVersion::new(&format!("sha256:{}", "b".repeat(64))).unwrap(),
        chunk_id: ChunkId::new("chk_TOTAL_LINE").unwrap(),
        observation_ids: vec![ObservationId::try_from("obs_TOTAL".to_owned()).unwrap()],
        embedding_version: EmbeddingVersion::new("emb_printed_en_1.0").unwrap(),
        collection: SemanticCollection::new(1, 1).unwrap(),
        retention_deadline_unix_seconds: 1_800_000_000,
    })
    .unwrap();

    let serialized = serde_json::to_value(&metadata).unwrap();

    assert_eq!(serialized["tenant_id"], "ten_ALPHA");
    assert_eq!(serialized["document_id"], "doc_INVOICE");
    assert_eq!(
        serialized["document_version"],
        format!("sha256:{}", "b".repeat(64))
    );
    assert_eq!(
        serialized["observation_ids"],
        serde_json::json!(["obs_TOTAL"])
    );
    for forbidden in [
        "text",
        "vector",
        "bucket",
        "object_name",
        "signed_url",
        "credential",
    ] {
        assert!(serialized.get(forbidden).is_none(), "exposed {forbidden}");
    }
    assert_eq!(
        serde_json::from_value::<VectorPointMetadata>(serialized).unwrap(),
        metadata
    );

    let mut unknown = serde_json::to_value(&metadata).unwrap();
    unknown["text"] = "untrusted content".into();
    assert!(serde_json::from_value::<VectorPointMetadata>(unknown).is_err());
    let mut cross_type = serde_json::to_value(&metadata).unwrap();
    cross_type["document_id"] = "mem_NOT_A_DOCUMENT".into();
    assert!(serde_json::from_value::<VectorPointMetadata>(cross_type).is_err());
    let mut duplicate_evidence = serde_json::to_value(&metadata).unwrap();
    duplicate_evidence["observation_ids"] = serde_json::json!(["obs_TOTAL", "obs_TOTAL"]);
    assert!(serde_json::from_value::<VectorPointMetadata>(duplicate_evidence).is_err());
    assert!(VectorPointMetadata::new(VectorPointMetadataInput {
        tenant_id: TenantId::new("ten_ALPHA").unwrap(),
        memory_record_id: MemoryRecordId::new("mem_INVOICE_TOTAL").unwrap(),
        memory_record_version: MemoryRecordVersion::new(&format!("sha256:{}", "a".repeat(64)))
            .unwrap(),
        document_id: DocumentId::new("doc_INVOICE").unwrap(),
        document_version: DocumentVersion::new(&format!("sha256:{}", "b".repeat(64))).unwrap(),
        chunk_id: ChunkId::new("chk_TOTAL_LINE").unwrap(),
        observation_ids: Vec::new(),
        embedding_version: EmbeddingVersion::new("emb_printed_en_1.0").unwrap(),
        collection: SemanticCollection::new(1, 1).unwrap(),
        retention_deadline_unix_seconds: 1_800_000_000,
    })
    .is_err());
}
