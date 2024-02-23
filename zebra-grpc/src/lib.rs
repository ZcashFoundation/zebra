//! Zebra gRPC interface.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_grpc")]

pub mod server;

#[cfg(test)]
mod tests;

/// The generated scanner proto
pub mod scanner {
    tonic::include_proto!("scanner");

    pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("scanner_descriptor");
}
