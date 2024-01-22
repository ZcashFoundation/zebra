//! Zebra gRPC interface.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_grpc")]

/// Tonic-generated traits and types
pub mod zebra_scan_service {
    tonic::include_proto!("zebra.scan.rpc"); // The string specified here must match the proto package name
}

mod auth;
mod init;
pub mod methods;
