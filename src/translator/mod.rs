pub mod microsoft;
pub mod openai_compatible;

use crate::config::ProviderConfig;
use crate::models::{TranslationRequest, TranslationResult};
pub use microsoft::MicrosoftTranslator;
pub use openai_compatible::OpenAICompatibleTranslator;

pub enum AnyTranslator {
    Microsoft(MicrosoftTranslator),
    OpenAICompatible(OpenAICompatibleTranslator),
}

impl AnyTranslator {
    pub async fn translate(
        &self,
        req: &TranslationRequest,
    ) -> Result<TranslationResult, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            Self::Microsoft(t) => t.translate(req).await,
            Self::OpenAICompatible(t) => t.translate(req).await,
        }
    }

    pub async fn test_connection(&self) -> (bool, String) {
        match self {
            Self::Microsoft(t) => t.test_connection().await,
            Self::OpenAICompatible(t) => t.test_connection().await,
        }
    }
}

pub fn create_translator(config: &ProviderConfig) -> AnyTranslator {
    match config.provider_type.as_str() {
        "microsoft" => AnyTranslator::Microsoft(MicrosoftTranslator::new(config.clone())),
        _ => AnyTranslator::OpenAICompatible(OpenAICompatibleTranslator::new(config.clone())),
    }
}
