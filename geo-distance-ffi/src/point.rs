use geo_bindgen_macro::GeoFfiType;

#[derive(Debug, Clone, Copy, PartialEq, GeoFfiType)]
#[geo_ffi(target = "crate::Coordinate")]
pub struct GeoPoint {
    #[geo_ffi(rename = "lat")]
    pub latitude: f64,
    #[geo_ffi(rename = "lon")]
    pub longitude: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Coordinate;

    #[test]
    fn round_trips_through_the_native_type() {
        let point = GeoPoint {
            latitude: 40.7128,
            longitude: -74.0060,
        };

        let native: Coordinate = point.into();
        assert_eq!(native.lat, point.latitude);
        assert_eq!(native.lon, point.longitude);

        let back: GeoPoint = native.into();
        assert_eq!(back, point);
    }
}
