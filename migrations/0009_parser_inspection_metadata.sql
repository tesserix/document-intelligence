set lock_timeout = '5s';
set statement_timeout = '30s';

alter table ocr_uploads
    add column parser_page_count integer check (parser_page_count between 1 and 300),
    add column parser_maximum_page_pixels bigint check (
        parser_maximum_page_pixels between 1 and 100000000
    ),
    add column parser_total_page_pixels bigint check (
        parser_total_page_pixels between 1 and 1000000000
    ),
    add column parser_profile text check (char_length(parser_profile) between 1 and 64),
    add column parser_version text check (char_length(parser_version) between 1 and 64),
    add constraint ocr_uploads_parser_inspection_shape check (
        (parser_page_count is null
            and parser_maximum_page_pixels is null
            and parser_total_page_pixels is null
            and parser_profile is null
            and parser_version is null)
        or
        (status = 'accepted'
            and parser_page_count is not null
            and parser_maximum_page_pixels is not null
            and parser_total_page_pixels is not null
            and parser_total_page_pixels >= parser_maximum_page_pixels
            and parser_profile is not null
            and parser_version is not null)
    ) not valid;

alter table ocr_uploads validate constraint ocr_uploads_parser_inspection_shape;
