do $$ begin
    if exists(select 1 from ocr_sandbox_sessions where upload_request is not null and purged_at is null) then
        raise exception 'drain sandbox cleanup before removing its durable session ledger';
    end if;
end $$;
drop table ocr_sandbox_sessions;
