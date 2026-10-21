//! Route matching.

use crate::config::{Route, Routes};
use store::FaxRecord;

/// Picks the first matching route.
#[derive(Debug, Clone, Default)]
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    /// Build from the configured routes (call [`Routes::validate`] first).
    pub fn new(routes: &Routes) -> Self {
        Router {
            routes: routes.routes.clone(),
        }
    }

    /// First route, in configuration order, that matches `rec` (`dnis`, `dnis_prefix` and `trunk`
    /// are ANDed when several are set on one route; `default = true` matches everything).
    pub fn find(&self, rec: &FaxRecord) -> Option<&Route> {
        self.routes.iter().find(|r| r.matches(rec))
    }

    /// The routes in order.
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::testutil::sample;

    fn routes(src: &str) -> Router {
        let r: Routes = toml::from_str(src).unwrap();
        r.validate().unwrap();
        Router::new(&r)
    }

    const D: &str = "[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/x\"\n";

    fn names(
        rt: &Router,
        mut rec: store::FaxRecord,
        dnis: Option<&str>,
        trunk: &str,
    ) -> Option<String> {
        rec.dnis = dnis.map(str::to_string);
        rec.trunk = trunk.into();
        rt.find(&rec).and_then(|r| r.name.clone())
    }

    #[test]
    fn d01_exact_prefix_trunk_default_and_none() {
        let src = format!(
            "[[routes]]\nname = \"exact\"\ndnis = \"025570001\"\n{D}\
             [[routes]]\nname = \"prefix\"\ndnis_prefix = \"2557\"\n{D}\
             [[routes]]\nname = \"prefix0\"\ndnis_prefix = \"02557\"\n{D}\
             [[routes]]\nname = \"trunk\"\ntrunk = \"cht\"\n{D}\
             [[routes]]\nname = \"both\"\ndnis_prefix = \"03\"\ntrunk = \"ac\"\n{D}"
        );
        let rt = routes(&src);
        let s = sample;
        assert_eq!(
            names(&rt, s(), Some("025570001"), "ac").as_deref(),
            Some("exact")
        );
        // Exact does not match a longer number; prefix 2557 does not match "02557..." (starts with 0).
        assert_eq!(
            names(&rt, s(), Some("0255700019"), "ac").as_deref(),
            Some("prefix0")
        );
        assert_eq!(
            names(&rt, s(), Some("25570002"), "ac").as_deref(),
            Some("prefix")
        );
        // Order matters: "prefix" (2557) wins over a later, also matching, route.
        assert_eq!(
            names(&rt, s(), Some("25570002"), "cht").as_deref(),
            Some("prefix")
        );
        assert_eq!(
            names(&rt, s(), Some("9999"), "cht").as_deref(),
            Some("trunk")
        );
        // Both conditions must hold.
        assert_eq!(names(&rt, s(), Some("0300"), "ac").as_deref(), Some("both"));
        assert_eq!(names(&rt, s(), Some("0300"), "other"), None);
        assert_eq!(names(&rt, s(), Some("9999"), "other"), None);
        // A fax without DNIS only matches trunk-only or default routes.
        assert_eq!(names(&rt, s(), None, "cht").as_deref(), Some("trunk"));
        assert_eq!(names(&rt, s(), None, "ac"), None);
        assert_eq!(rt.routes().len(), 5);
    }

    #[test]
    fn d01_default_route_catches_the_rest() {
        let src = format!(
            "[[routes]]\nname = \"a\"\ndnis = \"1\"\n{D}[[routes]]\nname = \"fallback\"\ndefault = true\n{D}"
        );
        let rt = routes(&src);
        assert_eq!(names(&rt, sample(), Some("1"), "t").as_deref(), Some("a"));
        assert_eq!(
            names(&rt, sample(), Some("2"), "t").as_deref(),
            Some("fallback")
        );
        assert_eq!(names(&rt, sample(), None, "t").as_deref(), Some("fallback"));
        assert!(Router::default().find(&sample()).is_none());
    }
}
