pub extern crate burrego;
extern crate kube;
extern crate wasmparser;

pub mod callback_handler;
pub mod callback_requests;
pub mod cluster_context;
pub mod constants;
pub(crate) mod policy;
pub mod policy_evaluator;
pub mod policy_evaluator_builder;
pub mod policy_metadata;
mod policy_tracing;
pub mod runtimes;
pub mod validation_response;

// API's that expose other crate types (such as Kubewarden Policy SDK
// or `policy_fetcher`) can either implement their own exposed types,
// and means to convert those types internally to their dependencies
// types, or depending on the specific case, re-export dependencies
// API's directly.
//
// Re-exporting specific crates that belong to us is easier for common
// consumers of these libraries along with the `policy-evaluator`, so
// they can access these crates through the `policy-evaluator` itself,
// streamlining their dependencies as well.
pub use policy_fetcher;
pub use policy_fetcher::kubewarden_policy_sdk::metadata::ProtocolVersion;
