set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    drop constraint ocr_uploads_parser_inspection_shape,
    drop column parser_version,
    drop column parser_profile,
    drop column parser_total_page_pixels,
    drop column parser_maximum_page_pixels,
    drop column parser_page_count;
