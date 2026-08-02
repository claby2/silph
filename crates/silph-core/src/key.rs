/// A wire-format metric key: a flat `<category>_<name>` field, optionally
/// qualified by an instance for metrics that exist per resource, e.g.
/// `disk_free:/home`. The name itself never contains `:`, so parsing splits on
/// the first occurrence and the instance may contain further colons.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricKey {
    pub name: String,
    pub instance: Option<String>,
}

impl MetricKey {
    pub fn new(name: impl Into<String>) -> Self {
        MetricKey {
            name: name.into(),
            instance: None,
        }
    }

    pub fn with_instance(name: impl Into<String>, instance: impl Into<String>) -> Self {
        MetricKey {
            name: name.into(),
            instance: Some(instance.into()),
        }
    }

    pub fn parse(s: &str) -> MetricKey {
        match s.split_once(':') {
            Some((name, instance)) => MetricKey::with_instance(name, instance),
            None => MetricKey::new(s),
        }
    }
}

impl std::fmt::Display for MetricKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.instance {
            Some(instance) => write!(f, "{}:{}", self.name, instance),
            None => f.write_str(&self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        let key = MetricKey::parse("cpu_user");
        assert_eq!(key, MetricKey::new("cpu_user"));
        assert_eq!(key.to_string(), "cpu_user");
    }

    #[test]
    fn parse_instanced_round_trip() {
        let key = MetricKey::parse("disk_free:/home");
        assert_eq!(key, MetricKey::with_instance("disk_free", "/home"));
        assert_eq!(key.to_string(), "disk_free:/home");
    }

    #[test]
    fn instance_may_contain_colons() {
        let key = MetricKey::parse("disk_free:/mnt/a:b");
        assert_eq!(key, MetricKey::with_instance("disk_free", "/mnt/a:b"));
        assert_eq!(MetricKey::parse(&key.to_string()), key);
    }
}
