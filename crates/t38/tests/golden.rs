//! I01: SpanDSP golden vectors. Every IFP packet SpanDSP 3.1.1 emitted during real T.38 fax
//! sessions (see `tests/vectors/spandsp/README.md`) must decode and re-encode byte-identically.

use std::collections::BTreeMap;
use std::path::PathBuf;
use t38::{DataType, FieldType, Ifp, T38Version};

const ALL: [&str; 8] = [
    "non_ecm_v0",
    "non_ecm_v3",
    "ecm_v0",
    "ecm_v3",
    "non_ecm_v0_v29",
    "ecm_v3_v29",
    "non_ecm_v0_v27ter",
    "ecm_v3_v27ter",
];

struct Record {
    from_answerer: bool,
    ifp: Vec<u8>,
}

fn load(name: &str) -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors/spandsp")
        .join(format!("{name}.ifpb"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let dir = bytes[i];
        let len = u32::from_be_bytes(bytes[i + 1..i + 5].try_into().unwrap()) as usize;
        out.push(Record {
            from_answerer: dir == 1,
            ifp: bytes[i + 5..i + 5 + len].to_vec(),
        });
        i += 5 + len;
    }
    assert_eq!(i, bytes.len(), "{name}: trailing garbage in bundle");
    out
}

fn check(name: &str, version: T38Version, min_packets: usize) -> BTreeMap<String, usize> {
    let recs = load(name);
    assert!(
        recs.len() >= min_packets,
        "{name}: only {} packets",
        recs.len()
    );
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut max_field = 0usize;
    for (n, r) in recs.iter().enumerate() {
        let ifp = Ifp::decode(&r.ifp, version)
            .unwrap_or_else(|e| panic!("{name} #{n}: decode failed: {e}; bytes {:02x?}", r.ifp));
        let re = ifp.encode(version).expect("re-encode");
        assert_eq!(
            re,
            r.ifp,
            "{name} #{n} ({}): re-encoded bytes differ from SpanDSP's",
            if r.from_answerer {
                "answerer"
            } else {
                "caller"
            }
        );
        let kind = match &ifp {
            Ifp::Indicator(i) => format!("indicator {i:?}"),
            Ifp::Data { data_type, fields } => {
                for f in fields {
                    max_field = max_field.max(f.data.len());
                }
                let types: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{:?}", f.field_type))
                    .collect();
                format!("data {data_type:?} [{}]", types.join(","))
            }
        };
        *kinds.entry(kind).or_default() += 1;
    }
    eprintln!(
        "I01 {name}: {} packets, {} distinct kinds, largest field {} octets, all byte-identical",
        recs.len(),
        kinds.len(),
        max_field
    );
    for (k, v) in &kinds {
        eprintln!("      {v:5} x {k}");
    }
    kinds
}

#[test]
fn i01_non_ecm_v0() {
    let k = check("non_ecm_v0", T38Version::V0, 600);
    assert!(k.keys().any(|s| s.contains("T4NonEcmData")));
    assert!(k.keys().any(|s| s.contains("T4NonEcmSigEnd")));
    assert!(k.keys().any(|s| s.contains("HdlcData")));
}

#[test]
fn i01_non_ecm_v3() {
    let k = check("non_ecm_v3", T38Version::V3, 600);
    assert!(k.keys().any(|s| s.contains("T4NonEcmData")));
}

#[test]
fn i01_ecm_v0() {
    let k = check("ecm_v0", T38Version::V0, 600);
    assert!(k.keys().any(|s| s.contains("HdlcFcsOkSigEnd")));
}

#[test]
fn i01_ecm_v3() {
    let k = check("ecm_v3", T38Version::V3, 600);
    assert!(k.keys().any(|s| s.contains("HdlcFcsOkSigEnd")));
}

#[test]
fn i01_non_ecm_v0_v29() {
    let k = check("non_ecm_v0_v29", T38Version::V0, 700);
    assert!(k.keys().any(|s| s.contains("V29_9600 [T4NonEcmData]") || s.contains("V29_7200 [T4NonEcmData]")), "{k:?}");
}

#[test]
fn i01_ecm_v3_v29() {
    let k = check("ecm_v3_v29", T38Version::V3, 700);
    assert!(k.keys().any(|s| s.contains("V29_")), "{k:?}");
}

#[test]
fn i01_non_ecm_v0_v27ter() {
    let k = check("non_ecm_v0_v27ter", T38Version::V0, 1000);
    assert!(k.keys().any(|s| s.contains("V27ter")), "{k:?}");
}

#[test]
fn i01_ecm_v3_v27ter() {
    let k = check("ecm_v3_v27ter", T38Version::V3, 1000);
    assert!(k.keys().any(|s| s.contains("V27ter")), "{k:?}");
}

/// The same octets also decode under the other version when no version-specific construct
/// is involved, and the v1/v2 syntax equals v3's (additions aside).
#[test]
fn i01_v3_vectors_decode_under_v1_and_v2() {
    for v in [T38Version::V1, T38Version::V2] {
        for r in load("ecm_v3") {
            let a = Ifp::decode(&r.ifp, v).unwrap();
            assert_eq!(a.encode(v).unwrap(), r.ifp);
        }
    }
}

/// The corpus exercises both modulation families the receiver cares about.
#[test]
fn i01_corpus_coverage() {
    let mut types = std::collections::BTreeSet::new();
    let mut fts = std::collections::BTreeSet::new();
    for name in ALL {
        let v = if name.contains("_v0") {
            T38Version::V0
        } else {
            T38Version::V3
        };
        for r in load(name) {
            if let Ifp::Data { data_type, fields } = Ifp::decode(&r.ifp, v).unwrap() {
                types.insert(format!("{data_type:?}"));
                for f in fields {
                    fts.insert(format!("{:?}", f.field_type));
                }
            }
        }
    }
    eprintln!("I01 data types seen: {types:?}\nI01 field types seen: {fts:?}");
    assert!(types.contains(&format!("{:?}", DataType::V21)));
    assert!(types.len() >= 4, "{types:?}");
    assert!(fts.contains(&format!("{:?}", FieldType::HdlcData)));
    assert!(fts.len() >= 5, "{fts:?}");
}
