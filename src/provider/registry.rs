use std::{collections::BTreeMap, fmt, sync::Arc};

use super::{ProviderFactory, ProviderId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    DuplicateProvider(ProviderId),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateProvider(id) => write!(formatter, "provider {id} is already registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Default)]
pub struct ProviderRegistry {
    factories: BTreeMap<ProviderId, Arc<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        factory: impl ProviderFactory + 'static,
    ) -> Result<(), RegistryError> {
        self.register_arc(Arc::new(factory))
    }

    pub fn register_arc(&mut self, factory: Arc<dyn ProviderFactory>) -> Result<(), RegistryError> {
        let id = factory.id();
        if self.factories.contains_key(&id) {
            return Err(RegistryError::DuplicateProvider(id));
        }
        self.factories.insert(id, factory);
        Ok(())
    }

    pub fn get(&self, id: &ProviderId) -> Option<&Arc<dyn ProviderFactory>> {
        self.factories.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ProviderId, &Arc<dyn ProviderFactory>)> {
        self.factories.iter()
    }

    pub fn len(&self) -> usize {
        self.factories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::provider::{
        CloudProvider, ProviderContext, ProviderIdentity, ProviderReadiness, ProviderResult,
    };

    struct FakeFactory;

    #[async_trait]
    impl ProviderFactory for FakeFactory {
        fn id(&self) -> ProviderId {
            ProviderId::new("fake").unwrap()
        }

        fn display_name(&self) -> &'static str {
            "Fake"
        }

        fn readiness(
            &self,
            _context: &ProviderContext,
            _identity: &ProviderIdentity,
        ) -> ProviderReadiness {
            ProviderReadiness::NeedsConfig
        }

        async fn create(
            &self,
            _context: ProviderContext,
            _identity: ProviderIdentity,
        ) -> ProviderResult<Arc<dyn CloudProvider>> {
            unreachable!("needs-config factories are not created")
        }
    }

    #[test]
    fn rejects_duplicate_provider_ids() {
        let mut registry = ProviderRegistry::new();
        registry.register(FakeFactory).unwrap();
        let error = registry.register(FakeFactory).unwrap_err();
        assert_eq!(
            error,
            RegistryError::DuplicateProvider(ProviderId::new("fake").unwrap())
        );
    }
}
