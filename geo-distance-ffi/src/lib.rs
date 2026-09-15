// The distance math and the nearest-match scan are done on the C++ side
// (`cpp/geo_distance.cc`) and called through a typed `cxx::bridge`. This is
// the kind of split that's worth it once you're scanning many candidate
// points per lookup: a tight, branch-free numeric loop in C++ instead of
// per-call FFI overhead for each pair.

#[cxx::bridge(namespace = "geo")]
mod ffi {
    #[derive(Debug, Clone, Copy)]
    struct Coordinate {
        lat: f64,
        lon: f64,
    }

    #[derive(Debug, Clone, Copy)]
    struct Match {
        candidate_index: usize,
        distance_km: f64,
    }

    unsafe extern "C++" {
        include!("geo-distance-ffi/include/geo_distance.h");

        fn haversine_distance_km(a: Coordinate, b: Coordinate) -> f64;

        /// For each origin, finds the nearest candidate and its distance.
        fn nearest_matches(origins: &[Coordinate], candidates: &[Coordinate]) -> Vec<Match>;
    }
}

mod point;

pub use point::GeoPoint;

pub use ffi::{Coordinate, Match};

pub fn haversine_km(a: Coordinate, b: Coordinate) -> f64 {
    ffi::haversine_distance_km(a, b)
}

pub fn nearest_matches(origins: &[Coordinate], candidates: &[Coordinate]) -> Vec<Match> {
    ffi::nearest_matches(origins, candidates)
}

pub fn nearest_matches_typed(origins: &[GeoPoint], candidates: &[GeoPoint]) -> Vec<Match> {
    let origins: Vec<Coordinate> = origins.iter().copied().map(Into::into).collect();
    let candidates: Vec<Coordinate> = candidates.iter().copied().map(Into::into).collect();
    ffi::nearest_matches(&origins, &candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_matches_known_distance() {
        // New York (JFK) to London (LHR), ~5,570 km great-circle.
        let jfk = Coordinate {
            lat: 40.6413,
            lon: -73.7781,
        };
        let lhr = Coordinate {
            lat: 51.4700,
            lon: -0.4543,
        };

        let distance = haversine_km(jfk, lhr);
        assert!(
            (distance - 5555.0).abs() < 50.0,
            "unexpected distance: {distance}"
        );
    }

    #[test]
    fn nearest_matches_picks_the_closest_candidate() {
        let origins = [Coordinate {
            lat: 40.7128,
            lon: -74.0060,
        }]; // NYC
        let candidates = [
            Coordinate {
                lat: 51.5072,
                lon: -0.1276,
            }, // London
            Coordinate {
                lat: 40.7357,
                lon: -74.1724,
            }, // Newark, NJ
        ];

        let matches = nearest_matches(&origins, &candidates);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].candidate_index, 1);
        assert!(matches[0].distance_km < 50.0);
    }

    #[test]
    fn nearest_matches_typed_agrees_with_the_raw_ffi_call() {
        let origins = [GeoPoint {
            latitude: 40.7128,
            longitude: -74.0060,
        }]; // NYC
        let candidates = [
            GeoPoint {
                latitude: 51.5072,
                longitude: -0.1276,
            }, // London
            GeoPoint {
                latitude: 40.7357,
                longitude: -74.1724,
            }, // Newark, NJ
        ];

        let typed = nearest_matches_typed(&origins, &candidates);

        let raw_origins: Vec<Coordinate> = origins.iter().copied().map(Into::into).collect();
        let raw_candidates: Vec<Coordinate> = candidates.iter().copied().map(Into::into).collect();
        let raw = nearest_matches(&raw_origins, &raw_candidates);

        assert_eq!(typed.len(), raw.len());
        assert_eq!(typed[0].candidate_index, raw[0].candidate_index);
        assert!((typed[0].distance_km - raw[0].distance_km).abs() < f64::EPSILON);
    }
}
