Calibration Matching Criteria For

Lights ([check box] Use BIAS for Dark Optimization)

| Type | instrume.   | binning.    | gain        | offset      | exptime     | focallen    | ccd temp               | 
|------|-------------|-------------|-------------|-------------|-------------|-------------|------------------------|
| Flat | Exact Match | Exact Match | Exact Match | Exact Match | -           | Exact Match | -                      |
| Dark | Exact Match | Exact Match | Exact Match | Exact Match | Exact Match | -           | Warning Threshold (2C) |
| Bias | Exact Match | Exact Match | Exact Match | Exact Match | -           | -           | Warning Threshold (2C) |

Flats ([check box] Use BIAS if no darks found) | ([check box] Use BIAS for Dark Optimization)

| Type | instrume.   | binning.    | gain        | offset      | exptime     | focallen    | ccd temp               |
|------|-------------|-------------|-------------|-------------|-------------|-------------|------------------------|
| Dark | Exact Match | Exact Match | Exact Match | Exact Match | Exact Match | -           | Warning Threshold (2C) |
| Bias | Exact Match | Exact Match | Exact Match | Exact Match | -           | -           | Warning Threshold (2C) |

Clustering parameters and Thresholds

| Setting Key | Default | Description |
|-------------|---------|-------------|
| `flats.max_age_days` | 30 | Maximum age of flats to consider valid |
| `flats.time_cluster_minutes` | 30 | Time threshold for clustering flat frames |
| `darks.max_age_days` | 30 | Maximum age of darks to consider valid |
| `darks.time_cluster_minutes` | 30 | Time threshold for clustering dark frames |
| `bias.max_age_days` | 30 | Maximum age of bias to consider valid |
| `bias.time_cluster_minutes` | 30 | Time threshold for clustering bias frames |
| `temperature.match_weight` | 0.3 | Weight for temperature proximity (0.0-1.0) |