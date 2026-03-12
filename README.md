# poros

CLI for querying Hong Kong MTR point-to-point journey times with fuzzy station matching.

```
poros "Quarry Bay" "Kowloon Bay"   # 13 min — Route: Quarry Bay → [Island] → Kowloon Bay
poros --matrix                      # Full N×N time table (TSV)
poros --refresh                     # Re-fetch from official MTR API
```

## How it works

Fetches station data from the [MTR Open Data API](https://opendata.mtr.com.hk/data/mtr_lines_and_stations.csv) and journey times from the official MTR journey planner endpoint. Results are cached locally at `~/.local/share/poros/cache.json`.

Route calculation uses BFS with transfer minimisation across all 10 MTR lines.

## Install

```bash
cargo install --path .
```

Requires Rust 2024 edition.
