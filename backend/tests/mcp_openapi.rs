use std::fs;

use serde_yaml_ng::Value;

#[test]
fn mcp_openapi_locks_crud_etags_and_secret_reference_schemas() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/openapi.yaml");
    let document: Value = serde_yaml_ng::from_slice(&fs::read(path).unwrap()).unwrap();
    let paths = document["paths"].as_mapping().unwrap();
    let collection = &paths["/api/v1/profiles/{profileId}/mcp/servers"];
    let item = &paths["/api/v1/profiles/{profileId}/mcp/servers/{serverId}"];
    assert!(collection["get"].is_mapping());
    assert!(collection["post"].is_mapping());
    assert!(item["patch"].is_mapping());
    assert!(item["delete"].is_mapping());
    assert!(!paths.contains_key("/api/v1/profiles/{profileId}/mcp/servers/{serverId}/test"));
    assert!(!paths.contains_key("/api/v1/profiles/{profileId}/mcp/servers/{serverId}/tools"));

    assert!(collection["get"]["responses"]["200"]["headers"]["ETag"].is_mapping());
    assert!(collection["post"]["responses"]["201"]["headers"]["ETag"].is_mapping());
    assert_eq!(
        collection["get"]["responses"]["200"]["headers"]["Cache-Control"]["schema"]["const"]
            .as_str(),
        Some("no-store")
    );
    assert_eq!(
        collection["post"]["responses"]["201"]["headers"]["Cache-Control"]["schema"]["const"]
            .as_str(),
        Some("no-store")
    );
    for operation in [&item["patch"], &item["delete"]] {
        let parameters = operation["parameters"].as_sequence().unwrap();
        assert!(parameters.iter().any(|parameter| {
            parameter["$ref"].as_str() == Some("#/components/parameters/IfMatch")
        }));
    }
    assert!(item["patch"]["responses"]["200"]["headers"]["ETag"].is_mapping());
    assert!(item["delete"]["responses"]["204"]["headers"]["ETag"].is_mapping());
    assert_eq!(
        item["patch"]["responses"]["200"]["headers"]["Cache-Control"]["schema"]["const"].as_str(),
        Some("no-store")
    );
    assert_eq!(
        item["delete"]["responses"]["204"]["headers"]["Cache-Control"]["schema"]["const"].as_str(),
        Some("no-store")
    );

    let schemas = &document["components"]["schemas"];
    let server = &schemas["McpServer"];
    let required = server["required"].as_sequence().unwrap();
    for field in [
        "envSecretNames",
        "bearerTokenSecretName",
        "missingSecretNames",
    ] {
        assert!(required.iter().any(|value| value.as_str() == Some(field)));
    }
    assert!(server["properties"]["secretNames"].is_null());
    assert!(schemas["McpTestResult"].is_null());
    assert!(schemas["McpTool"].is_null());
    assert_eq!(
        schemas["McpServerId"].as_str(),
        None,
        "server IDs are defined by the McpServer and parameter schemas, not a loose alias"
    );
    assert_eq!(
        document["components"]["parameters"]["McpServerId"]["schema"]["pattern"].as_str(),
        Some("^mcp_[0-9a-f]{32}$")
    );
}
