//! Validate basic Minecraft 1.7.10 entity NBT shapes without converting or
//! dropping modded/unknown tags. World import remains READ ONLY.
use fastnbt::Value;
use serde::Deserialize;
use std::io::{self, ErrorKind};

const MAX_ENTITIES_PER_CHUNK: usize = 4096;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Deserialize)]
struct EntityIdentity {
    id: String,
    #[serde(rename = "Pos")]
    pos: Vec<f64>,
}

/// Check the mandatory fields expected of stored Java 1.7.10 entities while
/// leaving the caller-owned original fastnbt::Value intact. The original
/// compound is never replaced by the narrow validation projection.
pub(crate) fn validate_entities(entities: &[Value]) -> io::Result<()> {
    if entities.len() > MAX_ENTITIES_PER_CHUNK {
        return Err(invalid("too many entities in an Anvil chunk"));
    }
    for original in entities {
        let bytes =
            fastnbt::to_bytes(original).map_err(|_| invalid("invalid entity NBT compound"))?;
        let entity: EntityIdentity =
            fastnbt::from_bytes(&bytes).map_err(|_| invalid("missing legacy entity identity or Pos"))?;
        if entity.id.is_empty()
            || entity.id.len() > 256
            || entity.pos.len() != 3
            || entity
                .pos
                .iter()
                .any(|position| !position.is_finite() || position.abs() > 30_000_000.0)
        {
            return Err(invalid("invalid legacy entity id or position"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct SampleEntity {
        id: String,
        #[serde(rename = "Pos")]
        pos: Vec<f64>,
        #[serde(rename = "Motion")]
        motion: Vec<f64>,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    }

    fn entity(id: &str, pos: Vec<f64>) -> Value {
        let mut extra = BTreeMap::new();
        extra.insert("ModdedEntityField".into(), Value::String("preserved".into()));
        let compound = SampleEntity {
            id: id.into(),
            pos,
            motion: vec![0.0, 0.0, 0.0],
            extra,
        };
        fastnbt::from_bytes(&fastnbt::to_bytes(&compound).unwrap()).unwrap()
    }

    #[test]
    fn accepts_legacy_entity_and_preserves_unknown_fields() {
        let value = entity("Zombie", vec![-0.5, 65.0, 32.5]);
        validate_entities(&[value.clone()]).unwrap();
        let bytes = fastnbt::to_bytes(&value).unwrap();
        let again: Value = fastnbt::from_bytes(&bytes).unwrap();
        assert!(matches!(again, Value::Compound(ref map)
            if matches!(map.get("ModdedEntityField"), Some(Value::String(value)) if value == "preserved")
        ));
    }

    #[test]
    fn rejects_malformed_positions_and_identity() {
        assert!(validate_entities(&[entity("", vec![1.0, 2.0, 3.0])]).is_err());
        assert!(validate_entities(&[entity("Pig", vec![1.0, 2.0])]).is_err());
        assert!(validate_entities(&[entity("Pig", vec![f64::NAN, 2.0, 3.0])]).is_err());
        assert!(validate_entities(&[entity("Pig", vec![30_000_001.0, 2.0, 3.0])]).is_err());
        assert!(validate_entities(&[Value::String("not an entity".into())]).is_err());
    }
}
