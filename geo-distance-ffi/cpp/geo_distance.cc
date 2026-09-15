#include "geo-distance-ffi/include/geo_distance.h"

#include <cmath>
#include <limits>

namespace geo {

namespace {

constexpr double kEarthRadiusKm = 6371.0088;

double to_radians(double degrees) { return degrees * M_PI / 180.0; }

}  // namespace

double haversine_distance_km(Coordinate a, Coordinate b) {
  const double lat1 = to_radians(a.lat);
  const double lat2 = to_radians(b.lat);
  const double dlat = lat2 - lat1;
  const double dlon = to_radians(b.lon - a.lon);

  const double sin_dlat = std::sin(dlat / 2.0);
  const double sin_dlon = std::sin(dlon / 2.0);

  const double h = sin_dlat * sin_dlat +
                    std::cos(lat1) * std::cos(lat2) * sin_dlon * sin_dlon;
  return 2.0 * kEarthRadiusKm * std::asin(std::sqrt(h));
}

rust::Vec<Match> nearest_matches(rust::Slice<const Coordinate> origins,
                                  rust::Slice<const Coordinate> candidates) {
  rust::Vec<Match> results;
  results.reserve(origins.size());

  for (const auto &origin : origins) {
    size_t best_index = 0;
    double best_distance = std::numeric_limits<double>::max();

    for (size_t i = 0; i < candidates.size(); ++i) {
      const double distance = haversine_distance_km(origin, candidates[i]);
      if (distance < best_distance) {
        best_distance = distance;
        best_index = i;
      }
    }

    results.push_back(Match{best_index, best_distance});
  }

  return results;
}

}  // namespace geo
