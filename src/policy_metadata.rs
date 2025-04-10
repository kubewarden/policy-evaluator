use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::{self, Display},
    path::Path,
};

use k8s_openapi::api::admissionregistration::v1::NamedRuleWithOperations;
use kubewarden_policy_sdk::metadata::ProtocolVersion;
use semver::Version;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};
use wasmparser::{Parser, Payload};

use crate::{errors::MetadataError, policy_evaluator::PolicyExecutionMode};

#[derive(Deserialize, Serialize, Debug, Clone, Hash, Eq, PartialEq)]
pub enum Operation {
    #[serde(rename = "CREATE")]
    Create,
    #[serde(rename = "UPDATE")]
    Update,
    #[serde(rename = "DELETE")]
    Delete,
    #[serde(rename = "CONNECT")]
    Connect,
    #[serde(rename = "*")]
    All,
}

impl TryFrom<&str> for Operation {
    type Error = &'static str;

    fn try_from(op: &str) -> Result<Self, Self::Error> {
        match op {
            "CREATE" => Ok(Operation::Create),
            "UPDATE" => Ok(Operation::Update),
            "DELETE" => Ok(Operation::Delete),
            "CONNECT" => Ok(Operation::Connect),
            "*" => Ok(Operation::All),
            _ => Err("unknown operation"),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    #[validate(length(min = 1), custom(function = "validate_asterisk_usage"))]
    pub api_groups: Vec<String>,
    #[validate(length(min = 1), custom(function = "validate_asterisk_usage"))]
    pub api_versions: Vec<String>,
    #[validate(length(min = 1), custom(function = "validate_resources"))]
    pub resources: Vec<String>,
    #[validate(
        length(min = 1),
        custom(function = "validate_asterisk_usage_inside_of_operations")
    )]
    pub operations: Vec<Operation>,
}

fn validate_asterisk_usage(data: &[String]) -> Result<(), ValidationError> {
    if data.contains(&String::from("*")) && data.len() > 1 {
        return Err(ValidationError::new(
            "No other elements can be defined when '*' is used",
        ));
    }
    Ok(())
}

fn validate_asterisk_usage_inside_of_operations(data: &[Operation]) -> Result<(), ValidationError> {
    if data.contains(&Operation::All) && data.len() > 1 {
        return Err(ValidationError::new(
            "No other elements can be defined when '*' is used",
        ));
    }
    Ok(())
}

fn validate_resources(data: &[String]) -> Result<(), ValidationError> {
    // This method is a transposition of the check done by Kubernetes
    // see https://github.com/kubernetes/kubernetes/blob/09268c16853b233ebaedcd6a877eac23690b5190/pkg/apis/admissionregistration/validation/validation.go#L44

    // */x
    let mut resources_with_wildcard_subresources: HashSet<String> = HashSet::new();
    // x/*
    let mut subresources_with_wildcard_resource: HashSet<String> = HashSet::new();
    // */*
    let mut has_double_wildcard = false;
    // *
    let mut has_single_wildcard = false;
    // x
    let mut has_resource_without_subresource = false;

    for resource in data.iter() {
        if resource.is_empty() {
            return Err(ValidationError::new("empty resource is not allowed"));
        }
        match resource.as_str() {
            "*/*" => has_double_wildcard = true,
            "*" => has_single_wildcard = true,
            _ => {}
        };

        let parts: Vec<&str> = resource.splitn(2, '/').collect();
        if parts.len() == 1 {
            has_resource_without_subresource = resource.as_str() != "*";
            continue;
        }
        let res = parts[0];
        let sub = parts[1];

        if resources_with_wildcard_subresources.contains(res) {
            let msg = format!("if '{resource}/*' is present, must not specify {res}");
            return Err(ValidationError::new(Box::leak(msg.into_boxed_str())));
        }
        if subresources_with_wildcard_resource.contains(sub) {
            let msg = format!("if '*/{sub}' is present, must not specify {resource}");
            return Err(ValidationError::new(Box::leak(msg.into_boxed_str())));
        }
        if sub == "*" {
            resources_with_wildcard_subresources.insert(String::from(res));
        }
        if res == "*" {
            subresources_with_wildcard_resource.insert(String::from(sub));
        }
    }
    if data.len() > 1 && has_double_wildcard {
        return Err(ValidationError::new(
            "if '*/*' is present, must not specify other resources",
        ));
    }
    if has_single_wildcard && has_resource_without_subresource {
        return Err(ValidationError::new(
            "if '*' is present, must not specify other resources without subresources",
        ));
    }

    Ok(())
}

impl TryFrom<&NamedRuleWithOperations> for Rule {
    type Error = &'static str;

    fn try_from(rule: &NamedRuleWithOperations) -> Result<Self, Self::Error> {
        let operations = match &rule.operations {
            Some(operations) => operations
                .iter()
                .map(|op| Operation::try_from(op.as_str()))
                .collect::<Result<Vec<Operation>, Self::Error>>()?,
            None => Vec::new(),
        };

        Ok(Rule {
            operations,
            api_groups: rule.api_groups.clone().unwrap_or_default(),
            api_versions: rule.api_versions.clone().unwrap_or_default(),
            resources: rule.resources.clone().unwrap_or_default(),
        })
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate, PartialEq, Hash, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct ContextAwareResource {
    #[validate(length(min = 1))]
    pub api_version: String,
    #[validate(length(min = 1))]
    pub kind: String,
}

impl From<&kubewarden_policy_sdk::crd::policies::common::ContextAwareResource>
    for ContextAwareResource
{
    fn from(resource: &kubewarden_policy_sdk::crd::policies::common::ContextAwareResource) -> Self {
        Self {
            api_version: resource.api_version.clone(),
            kind: resource.kind.clone(),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq)]
pub enum PolicyType {
    #[default]
    #[serde(rename = "kubernetes")]
    Kubernetes,
    #[serde(rename = "raw")]
    Raw,
}

impl Display for PolicyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(self).map_err(|_| fmt::Error {})?;
        write!(f, "{}", json.replace('"', ""))
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
#[serde(rename_all = "camelCase")]
#[validate(schema(function = "validate_metadata", skip_on_field_errors = false))]
pub struct Metadata {
    #[validate(required)]
    pub protocol_version: Option<ProtocolVersion>,
    #[validate(nested)]
    pub rules: Vec<Rule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
    pub mutating: bool,
    #[serde(default = "_default_true")]
    pub background_audit: bool,
    #[serde(default)]
    pub execution_mode: PolicyExecutionMode,
    #[serde(default)]
    pub policy_type: PolicyType,
    #[serde(default)]
    #[validate(nested)]
    pub context_aware_resources: BTreeSet<ContextAwareResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_kubewarden_version: Option<Version>,
}

const fn _default_true() -> bool {
    true
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            protocol_version: None,
            rules: vec![],
            annotations: Some(BTreeMap::new()),
            mutating: false,
            background_audit: true,
            execution_mode: PolicyExecutionMode::KubewardenWapc,
            policy_type: PolicyType::Kubernetes,
            context_aware_resources: BTreeSet::new(),
            minimum_kubewarden_version: None,
        }
    }
}

impl Metadata {
    pub fn from_path(path: &Path) -> std::result::Result<Option<Metadata>, MetadataError> {
        Metadata::from_contents(&std::fs::read(path).map_err(MetadataError::Path)?)
    }

    pub fn from_contents(policy: &[u8]) -> std::result::Result<Option<Metadata>, MetadataError> {
        for payload in Parser::new(0).parse_all(policy) {
            if let Payload::CustomSection(reader) = payload.map_err(MetadataError::WasmPayload)? {
                if reader.name() == crate::constants::KUBEWARDEN_CUSTOM_SECTION_METADATA {
                    return Ok(Some(serde_json::from_slice(reader.data()).map_err(
                        |e| MetadataError::Deserialize {
                            section: reader.name().to_string(),
                            error: e,
                        },
                    )?));
                }
            }
        }
        Ok(None)
    }
}

fn validate_metadata(metadata: &Metadata) -> Result<(), ValidationError> {
    if metadata.execution_mode == PolicyExecutionMode::KubewardenWapc
        && metadata.protocol_version == Some(ProtocolVersion::Unknown)
    {
        return Err(ValidationError::new(
            "Must specify a valid protocol version",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_json_diff::assert_json_eq;
    use serde_json::json;

    #[test]
    fn metadata_validation_pass() -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };
        let metadata = Metadata {
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };
        assert!(metadata.validate().is_ok());

        Ok(())
    }

    #[test]
    fn metadata_validation_failure() -> Result<(), ()> {
        // fail because api_groups has both '*' and another value
        let mut pod_rule = Rule {
            api_groups: vec![String::from(""), String::from("*")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };
        let protocol_version = Some(ProtocolVersion::V1);

        let mut metadata = Metadata {
            protocol_version,
            annotations: None,
            rules: vec![pod_rule],
            mutating: false,
            ..Default::default()
        };
        assert!(metadata.validate().is_err());

        // fail because api_group is empty
        pod_rule = Rule {
            api_groups: vec![],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };
        metadata.rules = vec![pod_rule];
        assert!(metadata.validate().is_err());

        // fail because operations has both '*' and another value
        pod_rule = Rule {
            api_groups: vec![String::from("")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::All, Operation::Create],
        };
        metadata.rules = vec![pod_rule];
        assert!(metadata.validate().is_err());

        // fails because there's no valid protocol version defined
        pod_rule = Rule {
            api_groups: vec![String::from("")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };
        metadata = Metadata {
            rules: vec![pod_rule],
            ..Default::default()
        };
        assert!(metadata.validate().is_err());

        pod_rule = Rule {
            api_groups: vec![String::from("")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };
        metadata = Metadata {
            rules: vec![pod_rule],
            ..Default::default()
        };
        assert!(metadata.validate().is_err());

        // fails because the protocol cannot be None
        metadata = Metadata {
            protocol_version: None,
            execution_mode: PolicyExecutionMode::KubewardenWapc,
            ..Default::default()
        };

        assert!(metadata.validate().is_err());

        Ok(())
    }

    #[test]
    fn metadata_with_kubewarden_execution_mode_must_have_a_valid_protocol() {
        let metadata = Metadata {
            protocol_version: Some(ProtocolVersion::Unknown),
            execution_mode: PolicyExecutionMode::KubewardenWapc,
            ..Default::default()
        };

        assert!(metadata.validate().is_err());

        let metadata = Metadata {
            protocol_version: Some(ProtocolVersion::V1),
            execution_mode: PolicyExecutionMode::KubewardenWapc,
            ..Default::default()
        };

        assert!(metadata.validate().is_ok());
    }

    #[test]
    fn metadata_with_rego_execution_mode_must_have_a_valid_protocol() {
        for mode in [PolicyExecutionMode::Opa, PolicyExecutionMode::OpaGatekeeper] {
            let metadata = Metadata {
                protocol_version: Some(ProtocolVersion::Unknown),
                execution_mode: mode,
                ..Default::default()
            };

            assert!(metadata.validate().is_ok());
        }
    }

    #[test]
    fn metadata_without_rules() -> Result<(), ()> {
        let metadata = Metadata {
            protocol_version: Some(ProtocolVersion::V1),
            annotations: None,
            ..Default::default()
        };

        let expected = json!({
            "protocolVersion": "v1",
            "rules": [ ],
            "mutating": false,
            "backgroundAudit": true,
            "contextAwareResources": [ ],
            "executionMode": "kubewarden-wapc",
            "policyType": "kubernetes"
        });

        let actual = serde_json::to_value(&metadata).unwrap();
        assert_json_eq!(expected, actual);
        Ok(())
    }

    #[test]
    fn metadata_backwards_compatibility() {
        // missing backgroundAudit on purpose
        // featuring the old `contextAware` boolean flag
        let json_metadata = json!({
            "protocolVersion": "v1",
            "rules": [ ],
            "mutating": false,
            "contextAware": true,
            "executionMode": "kubewarden-wapc",
        });

        let expected = Metadata {
            protocol_version: Some(ProtocolVersion::V1),
            annotations: None,
            background_audit: true,
            context_aware_resources: BTreeSet::new(),
            policy_type: PolicyType::Kubernetes,
            ..Default::default()
        };

        let actual: Metadata =
            serde_json::from_value(json_metadata).expect("cannot deserialize Metadata");
        assert_json_eq!(expected, actual);
    }

    #[test]
    fn metadata_init() -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("")],
            api_versions: vec![String::from("v1")],
            resources: vec![String::from("pods")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        let expected = json!(
        {
            "protocolVersion": "v1",
            "rules": [
                {
                    "apiGroups":[""],
                    "apiVersions":["v1"],
                    "resources":["pods"],
                    "operations":["CREATE"]
                }
            ],
            "annotations": {
                "io.kubewarden.policy.author": "Flavio Castelli"
            },
            "mutating": false,
            "backgroundAudit": true,
            "contextAwareResources": [ ],
            "executionMode": "kubewarden-wapc",
            "policyType": "kubernetes"
        });

        let actual = serde_json::to_value(&metadata).unwrap();
        assert_json_eq!(expected, actual);
        Ok(())
    }

    #[test]
    fn validate_resource_asterisk_can_coexist_with_resources_that_have_subresources(
    ) -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![
                String::from("*"),
                String::from("a/b"),
                String::from("a/*"),
                String::from("*/b"),
            ],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_ok());
        Ok(())
    }

    #[test]
    fn validate_resource_asterisk_cannot_mix_with_resources_that_do_not_have_subresources(
    ) -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![String::from("*"), String::from("a")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
        Ok(())
    }

    #[test]
    fn validate_resource_foo_slash_asterisk_subresource_cannot_mix_with_foo_slash_bar(
    ) -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![String::from("a/*"), String::from("a/x")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
        Ok(())
    }

    #[test]
    fn validate_resource_foo_slash_asterisk_can_mix_with_foo() -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![String::from("a/*"), String::from("a")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_ok());
        Ok(())
    }

    #[test]
    fn validate_resource_asterisk_slash_bar_cannot_mix_with_foo_slash_bar() -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![String::from("*/a"), String::from("x/a")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
        Ok(())
    }

    #[test]
    fn validate_resource_double_asterisk_cannot_mix_with_other_resources() -> Result<(), ()> {
        let pod_rule = Rule {
            api_groups: vec![String::from("a")],
            api_versions: vec![String::from("a")],
            resources: vec![String::from("*/*"), String::from("a")],
            operations: vec![Operation::Create],
        };

        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            rules: vec![pod_rule],
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
        Ok(())
    }

    #[test]
    fn validate_context_aware_resource_without_api_group() {
        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let mut context_aware_resources = BTreeSet::new();
        context_aware_resources.insert(ContextAwareResource {
            api_version: "".to_string(),
            kind: "Pod".to_string(),
        });

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            context_aware_resources,
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
    }

    #[test]
    fn validate_context_aware_resource_without_kind() {
        let mut annotations: BTreeMap<String, String> = BTreeMap::new();
        annotations.insert(
            String::from("io.kubewarden.policy.author"),
            String::from("Flavio Castelli"),
        );

        let mut context_aware_resources = BTreeSet::new();
        context_aware_resources.insert(ContextAwareResource {
            api_version: "v1".to_string(),
            kind: "".to_string(),
        });

        let metadata = Metadata {
            annotations: Some(annotations),
            protocol_version: Some(ProtocolVersion::V1),
            context_aware_resources,
            ..Default::default()
        };

        assert!(metadata.validate().is_err());
    }
}
