use std::borrow::Cow;

/// A wire-format metric key: a flat `<category>_<name>` field, optionally
/// qualified by an instance for metrics that exist per resource, e.g.
/// `disk_free:/home`. The name itself never contains `:`, so parsing splits on
/// the first occurrence and the instance may contain further colons.
///
/// `name` is a `Cow` because almost every key is built from a `&'static str`
/// literal in a metric's `collect`; borrowing keeps the per-scrape key
/// construction allocation-free for those.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricKey {
    pub name: Cow<'static, str>,
    pub instance: Option<String>,
}

impl MetricKey {
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        MetricKey {
            name: name.into(),
            instance: None,
        }
    }

    pub fn with_instance(name: impl Into<Cow<'static, str>>, instance: impl Into<String>) -> Self {
        MetricKey {
            name: name.into(),
            instance: Some(instance.into()),
        }
    }

    pub fn parse(s: &str) -> MetricKey {
        match s.split_once(':') {
            Some((name, instance)) => MetricKey::with_instance(name.to_owned(), instance),
            None => MetricKey::new(s.to_owned()),
        }
    }

    /// The wire key as an owned `String`, consuming the key. An unqualified
    /// key that already owns its name hands over that allocation instead of
    /// copying, and a borrowed one allocates exactly once — where
    /// `to_string()` on a borrowed name would allocate a second time.
    pub fn into_wire_key(self) -> String {
        match self.instance {
            None => self.name.into_owned(),
            Some(instance) => format!("{}:{}", self.name, instance),
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

    #[test]
    fn into_wire_key_matches_display() {
        for s in ["cpu_user", "disk_free:/home", "disk_free:/mnt/a:b"] {
            let key = MetricKey::parse(s);
            assert_eq!(key.clone().to_string(), key.into_wire_key());
        }
        assert_eq!(MetricKey::new("cpu_user").into_wire_key(), "cpu_user");
    }
}
