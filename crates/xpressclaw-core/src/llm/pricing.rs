use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Pricing for a model (per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

impl ModelPricing {
    pub fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_write: 0.0,
            cache_read: 0.0,
        }
    }

    pub fn with_cache(mut self, cache_write: f64, cache_read: f64) -> Self {
        self.cache_write = cache_write;
        self.cache_read = cache_read;
        self
    }
}

/// Calculates costs for different models.
pub struct PricingTable {
    pricing: HashMap<String, ModelPricing>,
    aliases: HashMap<String, String>,
}

impl Default for PricingTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PricingTable {
    pub fn new() -> Self {
        let mut pricing = HashMap::new();
        let mut aliases = HashMap::new();

        // Claude models (Anthropic)
        pricing.insert(
            "claude-opus-4-5-20251101".into(),
            ModelPricing::new(5.00, 25.00).with_cache(6.25, 0.50),
        );
        pricing.insert(
            "claude-opus-4-1-20250414".into(),
            ModelPricing::new(15.00, 75.00).with_cache(18.75, 1.50),
        );
        pricing.insert(
            "claude-opus-4-20250514".into(),
            ModelPricing::new(15.00, 75.00).with_cache(18.75, 1.50),
        );
        pricing.insert(
            "claude-sonnet-4-5-20251022".into(),
            ModelPricing::new(3.00, 15.00).with_cache(3.75, 0.30),
        );
        pricing.insert(
            "claude-sonnet-4-20250514".into(),
            ModelPricing::new(3.00, 15.00).with_cache(3.75, 0.30),
        );
        pricing.insert(
            "claude-haiku-4-5-20251022".into(),
            ModelPricing::new(1.00, 5.00).with_cache(1.25, 0.10),
        );
        pricing.insert(
            "claude-3-5-haiku-20241022".into(),
            ModelPricing::new(0.80, 4.00).with_cache(1.00, 0.08),
        );

        // OpenAI/GPT models
        pricing.insert("gpt-5.2".into(), ModelPricing::new(1.75, 14.00));
        pricing.insert("gpt-5-mini".into(), ModelPricing::new(0.25, 2.00));
        pricing.insert("gpt-4o".into(), ModelPricing::new(2.50, 10.00));
        pricing.insert("gpt-4o-mini".into(), ModelPricing::new(0.15, 0.60));

        // Local models (free)
        pricing.insert("local".into(), ModelPricing::new(0.0, 0.0));
        pricing.insert("Qwen/Qwen3-8B".into(), ModelPricing::new(0.0, 0.0));
        pricing.insert("Qwen/Qwen3.5-9B".into(), ModelPricing::new(0.0, 0.0));
        pricing.insert("qwen3:8b".into(), ModelPricing::new(0.0, 0.0));

        // Aliases
        aliases.insert("claude-opus-4.5".into(), "claude-opus-4-5-20251101".into());
        aliases.insert("claude-opus-4.1".into(), "claude-opus-4-1-20250414".into());
        aliases.insert("claude-opus-4".into(), "claude-opus-4-20250514".into());
        aliases.insert(
            "claude-sonnet-4.5".into(),
            "claude-sonnet-4-5-20251022".into(),
        );
        aliases.insert("claude-sonnet-4".into(), "claude-sonnet-4-20250514".into());
        aliases.insert(
            "claude-haiku-4.5".into(),
            "claude-haiku-4-5-20251022".into(),
        );
        aliases.insert(
            "claude-haiku-3.5".into(),
            "claude-3-5-haiku-20241022".into(),
        );

        Self { pricing, aliases }
    }

    fn resolve_model(&self, model: &str) -> String {
        if let Some(canonical) = self.aliases.get(model) {
            return canonical.clone();
        }
        if self.pricing.contains_key(model) {
            return model.to_string();
        }
        // Prefix matching
        for key in self.pricing.keys() {
            if model.starts_with(key) || key.starts_with(model) {
                return key.clone();
            }
        }
        model.to_string()
    }

    pub fn get_pricing(&self, model: &str) -> ModelPricing {
        let resolved = self.resolve_model(model);
        self.pricing
            .get(&resolved)
            .cloned()
            .unwrap_or(DEFAULT_PRICING)
    }

    pub fn calculate(
        &self,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache_creation_tokens: i64,
        cache_read_tokens: i64,
    ) -> f64 {
        let p = self.get_pricing(model);

        let input_cost = p.input * input_tokens as f64 / 1_000_000.0;
        let output_cost = p.output * output_tokens as f64 / 1_000_000.0;

        let cache_write_cost = if cache_creation_tokens > 0 {
            let price = if p.cache_write > 0.0 {
                p.cache_write
            } else {
                p.input
            };
            price * cache_creation_tokens as f64 / 1_000_000.0
        } else {
            0.0
        };

        let cache_read_cost = if cache_read_tokens > 0 && p.cache_read > 0.0 {
            p.cache_read * cache_read_tokens as f64 / 1_000_000.0
        } else {
            0.0
        };

        input_cost + output_cost + cache_write_cost + cache_read_cost
    }

    pub fn register(&mut self, model: String, pricing: ModelPricing) {
        self.pricing.insert(model, pricing);
    }

    /// Merge custom pricing from config. Custom entries override built-in ones.
    pub fn with_custom(mut self, custom: &HashMap<String, ModelPricing>) -> Self {
        for (model, pricing) in custom {
            self.pricing.insert(model.clone(), pricing.clone());
        }
        self
    }
}

/// Default pricing for unknown models (Haiku 4.5 rates).
const DEFAULT_PRICING: ModelPricing = ModelPricing {
    input: 1.00,
    output: 5.00,
    cache_write: 1.25,
    cache_read: 0.10,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_cost() {
        let table = PricingTable::new();

        // Claude Sonnet 4.5: $3/MTok in, $15/MTok out
        let cost = table.calculate("claude-sonnet-4-5-20251022", 1000, 500, 0, 0);
        // 1000 * 3.0 / 1M + 500 * 15.0 / 1M = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 1e-10);
    }

    #[test]
    fn test_alias_resolution() {
        let table = PricingTable::new();
        let cost_alias = table.calculate("claude-sonnet-4.5", 1000, 500, 0, 0);
        let cost_full = table.calculate("claude-sonnet-4-5-20251022", 1000, 500, 0, 0);
        assert!((cost_alias - cost_full).abs() < 1e-10);
    }

    #[test]
    fn test_local_model_free() {
        let table = PricingTable::new();
        let cost = table.calculate("local", 100_000, 50_000, 0, 0);
        assert!(cost.abs() < 1e-10);
    }

    #[test]
    fn test_cache_costs() {
        let table = PricingTable::new();
        // Claude Opus 4.5: cache_write=$6.25, cache_read=$0.50
        let cost = table.calculate("claude-opus-4-5-20251101", 0, 0, 1_000_000, 1_000_000);
        // 1M * 6.25/1M + 1M * 0.50/1M = 6.25 + 0.50 = 6.75
        assert!((cost - 6.75).abs() < 1e-10);
    }

    #[test]
    fn test_unknown_model_uses_default() {
        let table = PricingTable::new();
        let cost = table.calculate("unknown-model-xyz", 1_000_000, 0, 0, 0);
        // Default: $1.00/MTok input
        assert!((cost - 1.0).abs() < 1e-10);
    }
}
