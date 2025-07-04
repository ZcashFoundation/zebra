# Zebra Shielded Scanning gRPC Server

**This component is not supported anymore and has been deleted from the repository.**

## Get Started

### Setup

After setting up [Zebra Shielded Scanning](https://zebra.zfnd.org/user/shielded-scan.html), you can add a `listen-addr` argument to the scanner binary:


```bash
zebra-scanner --sapling-keys-to-scan '{"key":"zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz", "birthday_height": 419200}' --zebrad-cache-dir /media/alfredo/stuff/chain/zebra --zebra-rpc-listen-addr '127.0.0.1:8232' --listen-addr '127.0.0.1:8231'
```

Making requests to the server will also require a gRPC client, the examples here use `grpcurl`, though any gRPC client should work.

[See installation instructions for `grpcurl` here](https://github.com/fullstorydev/grpcurl?tab=readme-ov-file#installation).

The types can be accessed through the `zebra-grpc` crate's root `scanner` module for clients in a Rust environment, and the [`scanner.proto` file here](https://github.com/ZcashFoundation/zebra/blob/main/zebra-grpc/proto/scanner.proto) can be used to build types in other environments.

### Usage

To check that the gRPC server is running, try calling `scanner.Scanner/GetInfo`, for example with `grpcurl`:

```bash
grpcurl -plaintext '127.0.0.1:8231' scanner.Scanner/GetInfo
```

The response should look like:

```
{
  "minSaplingBirthdayHeight": 419200
}
```

An example request to the `Scan` method with `grpcurl` would look like:

```bash
grpcurl -plaintext -d '{ "keys": { "key": ["sapling_extended_full_viewing_key"] } }' '127.0.0.1:8231' scanner.Scanner/Scan
```

This will start scanning for transactions in Zebra's state and in new blocks as they're validated.

Or, to use the scanner gRPC server without streaming, try calling `RegisterKeys` with your Sapling extended full viewing key, waiting for the scanner to cache some results, then calling `GetResults`:

```bash
grpcurl -plaintext -d '{ "keys": { "key": ["sapling_extended_full_viewing_key"] } }' '127.0.0.1:8231' scanner.Scanner/RegisterKeys
grpcurl -plaintext -d '{ "keys": ["sapling_extended_full_viewing_key"] }' '127.0.0.1:8231' scanner.Scanner/GetResults
```

## gRPC Reflection

To see all of the provided methods with `grpcurl`, try:

```bash
grpcurl -plaintext '127.0.0.1:8231' list scanner.Scanner
```

This will list the paths to each method in the `Scanner` service:
```
scanner.Scanner.ClearResults
scanner.Scanner.DeleteKeys
scanner.Scanner.GetInfo
scanner.Scanner.GetResults
scanner.Scanner.RegisterKeys
```

To see the request and response types for a method, for example the `GetResults` method, try:


```bash
grpcurl -plaintext '127.0.0.1:8231' describe scanner.Scanner.GetResults \
&& grpcurl -plaintext '127.0.0.1:8231' describe scanner.GetResultsRequest \
&& grpcurl -plaintext '127.0.0.1:8231' describe scanner.GetResultsResponse \
&& grpcurl -plaintext '127.0.0.1:8231' describe scanner.Results \
&& grpcurl -plaintext '127.0.0.1:8231' describe scanner.Transactions \
&& grpcurl -plaintext '127.0.0.1:8231' describe scanner.Transaction
```

The response should be the request and response types for the `GetResults` method:

```
scanner.Scanner.GetResults is a method:
// Get all data we have stored for the given keys.
rpc GetResults ( .scanner.GetResultsRequest ) returns ( .scanner.GetResultsResponse );
scanner.GetResultsRequest is a message:
// A request for getting results for a set of keys.
message GetResultsRequest {
  // Keys for which to get results.
  repeated string keys = 1;
}
scanner.GetResultsResponse is a message:
// A set of responses for each provided key of a GetResults call.
message GetResultsResponse {
  // Results for each key.
  map<string, .scanner.Results> results = 1;
}
scanner.Results is a message:
// A result for a single key.
message Results {
  // A height, transaction id map
  map<uint32, .scanner.Transactions> by_height = 1;
}
scanner.Transactions is a message:
// A vector of transaction hashes
message Transactions {
  // Transactions
  repeated Transaction transactions = 1;
}
scanner.Transaction is a message:
// Transaction data
message Transaction {
  // The transaction hash/id
  string hash = 1;
}
```

## Methods

<!-- TODO: Add a reference to zebra-grpc method docs -->

---
#### GetInfo

Returns basic information about the `zebra-scan` instance.

#### RegisterKeys

Starts scanning for a set of keys, with optional start heights, and caching the results.
Cached results can later be retrieved by calling the `GetResults` or `Scan` methods.

#### DeleteKeys

Stops scanning transactions for a set of keys. Deletes the keys and their cached results for the keys from zebra-scan.

#### GetResults

Returns cached results for a set of keys.

#### ClearResults

Deletes any cached results for a set of keys.

#### Scan

Starts scanning for a set of keys and returns a stream of results.
