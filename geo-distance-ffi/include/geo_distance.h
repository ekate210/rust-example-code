#pragma once

#include "geo-distance-ffi/src/lib.rs.h"

namespace geo {

double haversine_distance_km(Coordinate a, Coordinate b);

rust::Vec<Match> nearest_matches(rust::Slice<const Coordinate> origins,
                                  rust::Slice<const Coordinate> candidates);

}  // namespace geo
