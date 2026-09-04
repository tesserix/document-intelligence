set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    drop constraint ocr_uploads_parser_page_geometry_shape,
    drop column parser_page_geometries;
