//! Endpoint Tests: Verifies Zebra's RPC and HTTP endpoints behavior.

#![allow(clippy::unwrap_in_result)]

use color_eyre::eyre::WrapErr;
use serde_json::Value;

use zebra_chain::parameters::Network::Mainnet;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;

use zebra_test::{args, net::random_known_port, prelude::*};

use crate::common::{
    config::{
        default_test_config, os_assigned_rpc_port_config, random_known_rpc_port_config,
        read_listen_addr_from_logs, testdir,
    },
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

#[cfg(all(feature = "prometheus", not(target_os = "windows")))]
#[tokio::test]
async fn metrics_endpoint() -> Result<()> {
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::io::Write;

    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url = format!("http://{endpoint}");

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config(&Mainnet)?;
    config.metrics.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let child = dir.spawn_child(args!["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    // Test metrics endpoint
    let res = client.get(url.try_into().expect("url is valid")).await;

    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let mut body = Vec::new();
    let mut body_stream = res.into_body();
    while let Some(next) = body_stream.frame().await {
        body.write_all(next?.data_ref().unwrap())?;
    }

    let (body, mut child) = child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(body))?;
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.any_output_line_contains(
        "# TYPE zebrad_build_info counter",
        &body,
        "metrics exporter response",
        "the metrics response header",
    )?;
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in metrics exporter response");

    // Make sure metrics was started
    output.stdout_line_contains(format!("Opened metrics endpoint at {endpoint}").as_str())?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

#[cfg(all(feature = "filter-reload", not(target_os = "windows")))]
#[tokio::test]
async fn tracing_endpoint() -> Result<()> {
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::io::Write;

    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url_default = format!("http://{endpoint}");
    let url_filter = format!("{url_default}/filter");

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config(&Mainnet)?;
    config.tracing.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let child = dir.spawn_child(args!["start"])?;

    // Run `zebrad` for a few seconds before testing the endpoint
    // Since we're an async function, we have to use a sleep future, not thread sleep.
    tokio::time::sleep(LAUNCH_DELAY).await;

    // Create an http client
    let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();

    // Test tracing endpoint
    let res = client
        .get(url_default.try_into().expect("url_default is valid"))
        .await;
    let (res, child) = child.kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let mut body = Vec::new();
    let mut body_stream = res.into_body();
    while let Some(next) = body_stream.frame().await {
        body.write_all(next?.data_ref().unwrap())?;
    }

    let (body, child) = child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(body))?;

    // Set a filter and make sure it was changed
    let request = hyper::Request::post(url_filter.clone())
        .body("zebrad=debug".to_string().into())
        .unwrap();

    let post = client.request(request).await;
    let (_post, child) = child.kill_on_error(post)?;

    let tracing_res = client
        .get(url_filter.try_into().expect("url_filter is valid"))
        .await;

    let (tracing_res, child) = child.kill_on_error(tracing_res)?;
    assert!(tracing_res.status().is_success());

    // Get the body of the response
    let mut tracing_body = Vec::new();
    let mut body_stream = tracing_res.into_body();
    while let Some(next) = body_stream.frame().await {
        tracing_body.write_all(next?.data_ref().unwrap())?;
    }

    let (tracing_body, mut child) =
        child.kill_on_error::<Vec<u8>, hyper::Error>(Ok(tracing_body.clone()))?;

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure tracing endpoint was started
    output.stdout_line_contains(format!("Opened tracing endpoint at {endpoint}").as_str())?;
    // TODO: Match some trace level messages from output

    // Make sure the endpoint header is correct
    // The header is split over two lines. But we don't want to require line
    // breaks at a specific word, so we run two checks for different substrings.

    output.any_output_line_contains(
        "HTTP endpoint allows dynamic control of the filter",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    output.any_output_line_contains(
        "tracing events",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in tracing filter response");

    // Make sure endpoint requests change the filter
    output.any_output_line_contains(
        "zebrad=debug",
        &tracing_body,
        "tracing filter endpoint response",
        "the modified tracing filter",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in modified tracing filter response");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with a single thread.
#[tokio::test]
async fn rpc_endpoint_single_thread() -> Result<()> {
    rpc_endpoint(false).await
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with multiple threads.
#[tokio::test]
async fn rpc_endpoint_parallel_threads() -> Result<()> {
    rpc_endpoint(true).await
}

/// Test that the JSON-RPC endpoint responds to a request.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
#[tracing::instrument]
async fn rpc_endpoint(parallel_cpu_threads: bool) -> Result<()> {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = os_assigned_rpc_port_config(parallel_cpu_threads, &Mainnet)?;

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Run `zebrad` for a few seconds before testing the endpoint
    std::thread::sleep(LAUNCH_DELAY);

    // Make the call to the `getinfo` RPC method
    let res = client.call("getinfo", "[]".to_string()).await?;

    // Test rpc endpoint response
    assert!(res.status().is_success());

    let body = res.bytes().await;
    let (body, mut child) = child.kill_on_error(body)?;

    let parsed: Value = serde_json::from_slice(&body)?;

    // Check that we have at least 4 characters in the `build` field.
    let build = parsed["result"]["build"].as_str().unwrap();
    assert!(build.len() > 4, "Got {build}");

    // Check that the `subversion` field has "Zebra" in it.
    let subversion = parsed["result"]["subversion"].as_str().unwrap();
    assert!(subversion.contains("Zebra"), "Got {subversion}");

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to requests with different content types.
///
/// This test ensures that the curl examples of zcashd rpc methods will also work in Zebra.
///
/// https://zcash.github.io/rpc/getblockchaininfo.html
#[tokio::test]
async fn rpc_endpoint_client_content_type() -> Result<()> {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = random_known_rpc_port_config(true, &Mainnet)?;

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?;

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Call to `getinfo` RPC method with a no content type.
    let res = client
        .call_with_no_content_type("getinfo", "[]".to_string())
        .await?;

    // Zebra will insert valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain`.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain` content type as the zcashd rpc docs.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain;".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain; other string` content type.
    let res = client
        .call_with_content_type(
            "getinfo",
            "[]".to_string(),
            "text/plain; other string".to_string(),
        )
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a valid `application/json` content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "application/json".to_string())
        .await?;

    // Zebra will not replace valid content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with invalid string as content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "whatever".to_string())
        .await?;

    // Zebra will not replace unrecognized content type and fail.
    assert!(res.status().is_client_error());

    Ok(())
}
