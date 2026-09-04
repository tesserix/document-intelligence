set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add column parser_page_geometries jsonb;

alter table ocr_uploads
    add constraint ocr_uploads_parser_page_geometry_shape check (
        parser_page_geometries is null
        or (
            status = 'accepted'
            and jsonb_typeof(parser_page_geometries) = 'array'
            and jsonb_array_length(parser_page_geometries) = parser_page_count
        )
    ) not valid;

alter table ocr_uploads validate constraint ocr_uploads_parser_page_geometry_shape;
