//! Generate an openapi.yaml file from the Zebra RPC methods

use std::{collections::HashMap, error::Error, fs::File, io::Write};

use quote::ToTokens;
use serde::Serialize;
use syn::LitStr;

use zebra_rpc::methods::{trees::GetTreestate, *};

// The API server
const SERVER: &str = "http://localhost:8232";

// The API methods
#[derive(Serialize, Debug)]
struct Methods {
    paths: HashMap<String, HashMap<String, MethodConfig>>,
}

// The configuration for each method
#[derive(Serialize, Clone, Debug)]
struct MethodConfig {
    tags: Vec<String>,
    description: String,
    #[serde(rename = "requestBody")]
    request_body: RequestBody,
    responses: HashMap<String, Response>,
}

// The request body
#[derive(Serialize, Clone, Debug)]
struct RequestBody {
    required: bool,
    content: Content,
}

// The content of the request body
#[derive(Serialize, Clone, Debug)]
struct Content {
    #[serde(rename = "application/json")]
    application_json: Application,
}

// The application of the request body
#[derive(Serialize, Clone, Debug)]
struct Application {
    schema: Schema,
}

// The schema of the request body
#[derive(Serialize, Clone, Debug)]
struct Schema {
    #[serde(rename = "type")]
    type_: String,
    properties: HashMap<String, Property>,
}

// The properties of the request body
#[derive(Serialize, Clone, Debug)]
struct Property {
    #[serde(rename = "type")]
    type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<ArrayItems>,
    default: String,
}

// The response
#[derive(Serialize, Clone, Debug)]
struct Response {
    description: String,
    content: Content,
}

// The array items
#[derive(Serialize, Clone, Debug)]
struct ArrayItems {}

fn main() -> Result<(), Box<dyn Error>> {
    let current_path = env!("CARGO_MANIFEST_DIR");

    // Define the paths to the Zebra RPC methods
    let paths = vec![
        (
            format!("{}/../zebra-rpc/src/methods.rs", current_path),
            "Rpc",
        ),
        (
            format!(
                "{}/../zebra-rpc/src/methods/get_block_template_rpcs.rs",
                current_path
            ),
            "GetBlockTemplateRpc",
        ),
    ];

    // Create a hashmap to store the method names and configuration
    let mut methods = HashMap::new();

    for zebra_rpc_methods_path in paths {
        // Read the source code from the file
        let source_code = std::fs::read_to_string(zebra_rpc_methods_path.0)?;

        // Parse the source code into a syn AST
        let syn_file = syn::parse_file(&source_code)?;

        // Create a hashmap to store the methods configuration
        let mut methods_config = HashMap::new();

        // Iterate over items in the file looking for traits
        for item in &syn_file.items {
            if let syn::Item::Trait(trait_item) = item {
                // Check if this trait is the one we're interested in
                if trait_item.ident == zebra_rpc_methods_path.1 {
                    // Iterate over the trait items looking for methods
                    for trait_item in &trait_item.items {
                        // Extract method name
                        let method_name = method_name(trait_item)?;

                        // Extract method documentation and description
                        let (method_doc, mut description) = method_doc(trait_item)?;

                        // Request type. TODO: All methods are POST so we just hardcode it
                        let request_type = "post".to_string();

                        // Tags. TODO: We are assuming 1 tag per call for now
                        let tags = tags(&method_doc)?;

                        // Parameters
                        let mut parameters_example = "[]".to_string();
                        if let Ok((params_description, params_example)) = get_params(&method_doc) {
                            // Add parameters to method description:
                            description =
                                add_params_to_description(&description, &params_description);
                            // The Zebra API uses a `params` array to pass arguments to the RPC methods,
                            // so we need to add this to the OpenAPI spec instead of `parameters`
                            parameters_example = params_example;
                        }

                        // Create the request body
                        let request_body = create_request_body(&method_name, &parameters_example);

                        // Check if we have parameters
                        let mut have_parameters = true;
                        if parameters_example == "[]" {
                            have_parameters = false;
                        }

                        // Create the responses
                        let responses = create_responses(&method_name, have_parameters)?;

                        // Add the method configuration to the hashmap
                        methods_config.insert(
                            request_type,
                            MethodConfig {
                                tags,
                                description,
                                request_body,
                                responses,
                            },
                        );

                        // Add the method name and configuration to the hashmap
                        methods.insert(format!("/{}", method_name), methods_config.clone());
                    }
                }
            }
        }
    }

    // Create a struct to hold all the methods
    let all_methods = Methods { paths: methods };

    // Add openapi header and write to file
    let yaml_string = serde_yaml::to_string(&all_methods)?;
    let mut w = File::create("openapi.yaml")?;
    w.write_all(format!("{}{}", create_yaml(), yaml_string).as_bytes())?;

    Ok(())
}

// Create the openapi.yaml header
fn create_yaml() -> String {
    format!("openapi: 3.0.3
info:
    title: Swagger Zebra API - OpenAPI 3.0
    version: 0.0.1
    description: |-
        This is the Zebra API. It is a JSON-RPC 2.0 API that allows you to interact with the Zebra node.

        Useful links:
        - [The Zebra repository](https://github.com/ZcashFoundation/zebra)
        - [The latests API spec](https://github.com/ZcashFoundation/zebra/blob/main/openapi.yaml)
servers:
  - url: {}
", SERVER)
}

// Extract the method name from the trait item
fn method_name(trait_item: &syn::TraitItem) -> Result<String, Box<dyn Error>> {
    let mut method_name = "".to_string();
    if let syn::TraitItem::Fn(method) = trait_item {
        method_name = method.sig.ident.to_string();

        // Refine name if needed
        method.attrs.iter().for_each(|attr| {
            if attr.path().is_ident("rpc") {
                let _ = attr.parse_nested_meta(|meta| {
                    method_name = meta.value()?.parse::<LitStr>()?.value();
                    Ok(())
                });
            }
        });
    }
    Ok(method_name)
}

// Return the method docs array and the description of the method
fn method_doc(method: &syn::TraitItem) -> Result<(Vec<String>, String), Box<dyn Error>> {
    let mut method_doc = vec![];
    if let syn::TraitItem::Fn(method) = method {
        // Filter only doc attributes
        let doc_attrs: Vec<_> = method
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("doc"))
            .collect();

        // If no doc attributes found, return an error
        if doc_attrs.is_empty() {
            return Err("No documentation attribute found for the method".into());
        }

        method.attrs.iter().for_each(|attr| {
            if attr.path().is_ident("doc") {
                method_doc.push(attr.to_token_stream().to_string());
            }
        });
    }

    // Extract the description from the first line of documentation
    let description = match method_doc[0].split_once('"') {
        Some((_, desc)) => desc.trim().to_string().replace('\'', "''"),
        None => return Err("Description not found in method documentation".into()),
    };

    Ok((method_doc, description))
}

// Extract the tags from the method documentation. TODO: Assuming 1 tag per method for now
fn tags(method_doc: &[String]) -> Result<Vec<String>, Box<dyn Error>> {
    // Find the line containing tags information
    let tags_line = method_doc
        .iter()
        .find(|line| line.contains("tags:"))
        .ok_or("Tags not found in method documentation")?;

    // Extract tags from the tags line
    let mut tags = Vec::new();
    let tags_str = tags_line
        .split(':')
        .nth(1)
        .ok_or("Invalid tags line")?
        .trim();

    // Split the tags string into individual tags
    for tag in tags_str.split(',') {
        let trimmed_tag = tag.trim_matches(|c: char| !c.is_alphanumeric());
        if !trimmed_tag.is_empty() {
            tags.push(trimmed_tag.to_string());
        }
    }

    Ok(tags)
}

// Extract the parameters from the method documentation
fn get_params(method_doc: &[String]) -> Result<(String, String), Box<dyn Error>> {
    // Find the start and end index of the parameters
    let params_start_index = method_doc
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains("# Parameters"));
    let notes_start_index = method_doc
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains("# Notes"));

    // If start and end indices of parameters are found, extract them
    if let (Some((params_index, _)), Some((notes_index, _))) =
        (params_start_index, notes_start_index)
    {
        let params = &method_doc[params_index + 2..notes_index - 1];

        // Initialize variables to store parameter descriptions and examples
        let mut param_descriptions = Vec::new();
        let mut param_examples = Vec::new();

        // Iterate over the parameters and extract information
        for param_line in params {
            // Check if the line starts with the expected format
            if param_line.trim().starts_with("# [doc = \" -") {
                // Extract parameter name and description
                if let Some((name, description)) = extract_param_info(param_line) {
                    param_descriptions.push(format!("- `{}` - {}", name, description));

                    // Extract parameter example if available
                    if let Some(example) = extract_param_example(param_line) {
                        param_examples.push(example);
                    }
                }
            }
        }

        // Format parameters and examples
        let params_formatted = format!("[{}]", param_examples.join(", "));
        let params_description = param_descriptions.join("\n");

        return Ok((params_description, params_formatted));
    }

    Err("No parameters found".into())
}

// Extract parameter name and description
fn extract_param_info(param_line: &str) -> Option<(String, String)> {
    let start_idx = param_line.find('`')?;
    let end_idx = param_line.rfind('`')?;
    let name = param_line[start_idx + 1..end_idx].trim().to_string();

    let description_starts = param_line.find(") ")?;
    let description_ends = param_line.rfind("\"]")?;
    let description = param_line[description_starts + 2..description_ends]
        .trim()
        .to_string();

    Some((name, description))
}

// Extract parameter example if available
fn extract_param_example(param_line: &str) -> Option<String> {
    if let Some(example_start) = param_line.find("example=") {
        let example_ends = param_line.rfind(')')?;
        let example = param_line[example_start + 8..example_ends].trim();
        Some(example.to_string())
    } else {
        None
    }
}

// Create the request body
fn create_request_body(method_name: &str, parameters_example: &str) -> RequestBody {
    // Add the method name to the request body
    let method_name_prop = Property {
        type_: "string".to_string(),
        items: None,
        default: method_name.to_string(),
    };

    // Add a hardcoded request_id to the request body
    let request_id_prop = Property {
        type_: "number".to_string(),
        items: None,
        default: "123".to_string(),
    };

    // Create the schema and add the first 2 properties
    let mut schema = HashMap::new();
    schema.insert("method".to_string(), method_name_prop);
    schema.insert("id".to_string(), request_id_prop);

    // Add the parameters with the extracted examples
    let default = parameters_example.replace('\\', "");
    schema.insert(
        "params".to_string(),
        Property {
            type_: "array".to_string(),
            items: Some(ArrayItems {}),
            default,
        },
    );

    // Create the request body
    let content = Content {
        application_json: Application {
            schema: Schema {
                type_: "object".to_string(),
                properties: schema,
            },
        },
    };

    RequestBody {
        required: true,
        content,
    }
}

// Create the responses
fn create_responses(
    method_name: &str,
    have_parameters: bool,
) -> Result<HashMap<String, Response>, Box<dyn Error>> {
    let mut responses = HashMap::new();

    let properties = get_default_properties(method_name)?;

    let res_ok = Response {
        description: "OK".to_string(),
        content: Content {
            application_json: Application {
                schema: Schema {
                    type_: "object".to_string(),
                    properties,
                },
            },
        },
    };
    responses.insert("200".to_string(), res_ok);

    let mut properties = HashMap::new();
    if have_parameters {
        properties.insert(
            "error".to_string(),
            Property {
                type_: "string".to_string(),
                items: None,
                default: "Invalid parameters".to_string(),
            },
        );
        let res_bad_request = Response {
            description: "Bad request".to_string(),
            content: Content {
                application_json: Application {
                    schema: Schema {
                        type_: "object".to_string(),
                        properties,
                    },
                },
            },
        };
        responses.insert("400".to_string(), res_bad_request);
    }

    Ok(responses)
}

// Add the parameters to the method description
fn add_params_to_description(description: &str, params_description: &str) -> String {
    let mut new_description = description.to_string();
    new_description.push_str("\n\n**Request body `params` arguments:**\n\n");
    new_description.push_str(params_description);
    new_description
}

// Get requests examples by using defaults from the Zebra RPC methods
fn get_default_properties(method_name: &str) -> Result<HashMap<String, Property>, Box<dyn Error>> {
    // TODO: Complete the list of methods

    let type_ = "object".to_string();
    let items = None;
    let mut props = HashMap::new();

    let properties = match method_name {
        "getinfo" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetInfo::default())?,
                },
            );
            props
        }
        "getbestblockhash" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetBlockHash::default())?,
                },
            );
            props
        }
        "getblockchaininfo" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetBlockChainInfo::default())?,
                },
            );
            props
        }
        "getblock" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetBlock::default())?,
                },
            );
            props
        }
        "getblockhash" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetBlockHash::default())?,
                },
            );
            props
        }
        "z_gettreestate" => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items,
                    default: serde_json::to_string(&GetTreestate::default())?,
                },
            );
            props
        }
        _ => {
            props.insert(
                "result".to_string(),
                Property {
                    type_,
                    items: None,
                    default: "{}".to_string(),
                },
            );
            props
        }
    };
    Ok(properties)
}
