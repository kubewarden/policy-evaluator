use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct EvaluationResult {
    pub result: bool,
}
