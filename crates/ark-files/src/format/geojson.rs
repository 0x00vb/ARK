use crate::{Content, Edit, Error, Format, Kind};

pub struct GeoJson;

fn valid(bytes: &[u8]) -> Result<(), Error> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::Format("Map source must be UTF-8 JSON".into()))?;
    if !matches!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some(
            "FeatureCollection"
                | "Feature"
                | "Point"
                | "MultiPoint"
                | "LineString"
                | "MultiLineString"
                | "Polygon"
                | "MultiPolygon"
                | "GeometryCollection"
        )
    ) {
        return Err(Error::Format("Map source needs a GeoJSON object".into()));
    }
    Ok(())
}

impl Format for GeoJson {
    fn name(&self) -> &'static str {
        "geojson"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["geojson"]
    }
    fn kind(&self) -> Kind {
        Kind::Map
    }
    fn read(&self, bytes: &[u8]) -> Result<Content, Error> {
        valid(bytes)?;
        super::Text.read(bytes)
    }
    fn edit(&self, bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, Error> {
        let bytes = super::Text.edit(bytes, edits)?;
        valid(&bytes)?;
        Ok(bytes)
    }
    fn create(&self, content: &Content) -> Result<Vec<u8>, Error> {
        let bytes = super::Text.create(content)?;
        valid(&bytes)?;
        Ok(bytes)
    }
}
