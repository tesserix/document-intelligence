mod api;
pub mod cleanup;
mod config;
mod error;
mod storage;
mod store;
mod upstream;
mod web;
pub use api::{router, App};
pub use config::Config;
pub use error::Error;
pub use store::Store;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_identity_cannot_impersonate_a_production_product() {
        assert!(super::Config::validate_identity(
            "demo-v1=ocr-sandbox:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_ok());
        assert!(super::Config::validate_identity(
            "demo-v1=kora:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_err());
    }
}
