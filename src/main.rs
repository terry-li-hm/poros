use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = "Queries MTR (Hong Kong subway) point-to-point journey times.")]
struct Args {
    /// Departure station (fuzzy matching, case-insensitive)
    #[arg(value_name = "FROM")]
    from: Option<String>,

    /// Destination station (fuzzy matching, case-insensitive)
    #[arg(value_name = "TO")]
    to: Option<String>,

    /// Re-scrapes official MTR API and updates cache
    #[arg(long)]
    refresh: bool,

    /// Prints full N×N table as TSV to stdout
    #[arg(long)]
    matrix: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Cache {
    stations: Vec<String>,
    // matrix[from_station][to_station] = minutes
    matrix: HashMap<String, HashMap<String, u32>>,
    #[serde(default)]
    network: Vec<(String, Vec<String>)>,
}

fn fetch_mtr_data() -> Result<(Vec<(String, Vec<String>)>, HashMap<String, u32>), String> {
    let url = "https://opendata.mtr.com.hk/data/mtr_lines_and_stations.csv";
    let resp = reqwest::blocking::get(url).map_err(|e| e.to_string())?;
    let mut text = resp.text().map_err(|e| e.to_string())?;

    if text.starts_with('\u{feff}') {
        text.remove(0);
    }

    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let mut line_map: HashMap<String, Vec<(f32, String)>> = HashMap::new();
    let mut id_map: HashMap<String, u32> = HashMap::new();

    for result in rdr.records() {
        let record = result.map_err(|e| e.to_string())?;
        if record.len() < 7 { continue; }
        let line_code = &record[0];
        let direction = &record[1];
        let station_id_str = &record[2];
        let english_name = &record[5];
        let sequence_str = &record[6];

        if direction != "DT" {
            continue;
        }

        let station_id: u32 = station_id_str.parse().unwrap_or(0);
        id_map.insert(english_name.to_string(), station_id);

        let sequence: f32 = sequence_str.parse().unwrap_or(0.0);
        line_map
            .entry(line_code.to_string())
            .or_default()
            .push((sequence, english_name.to_string()));
    }

    let mut network = Vec::new();
    let display_names = vec![
        ("AEL", "Airport Express"),
        ("DRL", "Disneyland Resort"),
        ("EAL", "East Rail"),
        ("ISL", "Island"),
        ("KTL", "Kwun Tong"),
        ("SIL", "South Island"),
        ("TCL", "Tung Chung"),
        ("TKL", "Tseung Kwan O"),
        ("TML", "Tuen Ma"),
        ("TWL", "Tsuen Wan"),
    ];

    for (code, display) in display_names {
        if let Some(mut stations) = line_map.remove(code) {
            stations.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let names: Vec<String> = stations.into_iter().map(|(_, name)| name).collect();
            network.push((display.to_string(), names));
        }
    }

    Ok((network, id_map))
}

fn get_cache_path() -> PathBuf {
    let home_dir = std::env::var("HOME").expect("HOME environment variable not set");
    let mut path = PathBuf::from(home_dir);
    path.push(".local/share/poros/cache.json");
    path
}

fn scrape(id_map: &HashMap<String, u32>) -> Result<Cache, String> {
    let mut cache = Cache::default();
    let mut station_names: Vec<String> = id_map.keys().cloned().collect();
    station_names.sort();
    cache.stations = station_names.clone();

    let total_stations = station_names.len();
    println!("Scraping journey times for {} unique stations (DT direction)...", total_stations);

    for (i, from_name) in station_names.iter().enumerate() {
        if i % 10 == 0 {
            println!("Progress: {}/{} stations processed...", i, total_stations);
        }

        let from_id = id_map.get(from_name).unwrap();
        let mut row = HashMap::new();

        for to_name in &station_names {
            if from_name == to_name {
                row.insert(to_name.clone(), 0);
                continue;
            }

            let to_id = id_map.get(to_name).unwrap();
            let url = format!("https://www.mtr.com.hk/share/customer/jp/api/HRRoutes/?o={}&d={}", from_id, to_id);
            
            // Be polite: 500ms sleep
            std::thread::sleep(std::time::Duration::from_millis(50));

            let resp = reqwest::blocking::get(&url).map_err(|e| e.to_string())?;
            let data: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

            if data["errorCode"] == "0" {
                if let Some(routes) = data["routes"].as_array() {
                    if !routes.is_empty() {
                        if let Some(time) = routes[0]["time"].as_u64() {
                            row.insert(to_name.clone(), time as u32);
                        }
                    }
                }
            }
        }
        cache.matrix.insert(from_name.clone(), row);
    }

    println!("\nScraping complete.");
    Ok(cache)
}

fn save_cache(cache: &Cache) -> Result<(), String> {
    let path = get_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }
    let json = serde_json::to_string_pretty(cache).map_err(|e| format!("Failed to serialize cache: {}", e))?;
    fs::write(path, json).map_err(|e| format!("Failed to write cache file: {}", e))?;
    Ok(())
}

fn load_cache() -> Result<Cache, String> {
    let path = get_cache_path();
    if !path.exists() {
        return Err("Cache file missing".to_string());
    }
    let json = fs::read_to_string(path).map_err(|e| format!("Failed to read cache file: {}", e))?;
    let cache: Cache = serde_json::from_str(&json).map_err(|e| format!("Failed to parse cache: {}", e))?;
    Ok(cache)
}

fn fuzzy_match<'a>(query: &str, stations: &'a [String]) -> Result<&'a String, String> {
    let query_lower = query.to_lowercase();
    
    // Exact match first
    if let Some(s) = stations.iter().find(|s| s.to_lowercase() == query_lower) {
        return Ok(s);
    }
    
    let matches: Vec<&String> = stations
        .iter()
        .filter(|s| s.to_lowercase().contains(&query_lower))
        .collect();

    match matches.len() {
        0 => {
            let mut sorted_stations = stations.to_vec();
            sorted_stations.sort();
            Err(format!(
                "Station '{}' not found. Available stations:\n{}",
                query,
                sorted_stations.join(", ")
            ))
        },
        1 => Ok(matches[0]),
        _ => Err(format!(
            "Ambiguous station name '{}'. Matches: {}",
            query,
            matches
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
}

fn find_route(from: &str, to: &str, cache_stations: &[String], network: &[(String, Vec<String>)]) -> String {
    let mut norm_to_canonical = HashMap::new();
    for s in cache_stations {
        norm_to_canonical.insert(normalize(s), s.clone());
    }

    let mut graph: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (line_name, stations) in network {
        for i in 0..stations.len() {
            let s1_norm = normalize(&stations[i]);
            let s1 = if let Some(s) = norm_to_canonical.get(&s1_norm) { s.clone() } else { continue };
            if i > 0 {
                let s2_norm = normalize(&stations[i - 1]);
                if let Some(s2) = norm_to_canonical.get(&s2_norm) {
                    graph.entry(s1.clone()).or_default().push((s2.clone(), line_name.clone()));
                    graph.entry(s2.clone()).or_default().push((s1.clone(), line_name.clone()));
                }
            }
        }
    }

    let mut queue = VecDeque::new();
    let from_norm = normalize(from);
    let from_canonical = norm_to_canonical.get(&from_norm).cloned().unwrap_or_else(|| from.to_string());

    let mut visited = HashMap::new(); // (station, line) -> transfers

    for (line_name, stations) in network {
        if stations.iter().any(|s| normalize(s) == from_norm) {
            let state = (from_canonical.clone(), line_name.clone());
            queue.push_back((from_canonical.clone(), line_name.clone(), 0, vec![state.clone()]));
            visited.insert(state, 0);
        }
    }

    let mut best_path: Option<Vec<(String, String)>> = None;
    let mut min_transfers = u32::MAX;

    while let Some((curr_station, curr_line, transfers, path)) = queue.pop_front() {
        if transfers > min_transfers { continue; }

        if normalize(&curr_station) == normalize(to) {
            if transfers < min_transfers {
                min_transfers = transfers;
                best_path = Some(path);
            }
            continue;
        }

        // 0-transfer: same line
        if let Some(neighbors) = graph.get(&curr_station) {
            for (next_station, next_line) in neighbors {
                if next_line == &curr_line {
                    let next_state = (next_station.clone(), next_line.clone());
                    if !visited.contains_key(&next_state) || visited[&next_state] > transfers {
                        visited.insert(next_state.clone(), transfers);
                        let mut next_path = path.clone();
                        next_path.push(next_state.clone());
                        queue.push_back((next_station.clone(), next_line.clone(), transfers, next_path));
                    }
                }
            }
        }

        // 1-transfer: change line
        for (line_name, stations) in network {
            if line_name != &curr_line && stations.iter().any(|s| normalize(s) == normalize(&curr_station)) {
                let next_transfers = transfers + 1;
                let next_state = (curr_station.clone(), line_name.clone());
                if !visited.contains_key(&next_state) || visited[&next_state] > next_transfers {
                    visited.insert(next_state.clone(), next_transfers);
                    let mut next_path = path.clone();
                    next_path.push(next_state.clone());
                    queue.push_back((curr_station.clone(), line_name.clone(), next_transfers, next_path));
                }
            }
        }
    }

    if let Some(path) = best_path {
        if path.is_empty() { return "unknown".to_string(); }
        let mut result = Vec::new();
        result.push(path[0].0.clone());
        let mut current_line = path[0].1.clone();
        let clean_line = |l: &str| {
            l.replace(" (Lok Ma Chau)", "")
                .replace(" (Racecourse)", "")
                .replace(" (Lohas Park)", "")
                .replace(" Line", "")
        };
        result.push(format!("[{}]", clean_line(&current_line)));
        for i in 1..path.len() {
            if path[i].1 != current_line {
                result.push(path[i].0.clone());
                current_line = path[i].1.clone();
                result.push(format!("[{}]", clean_line(&current_line)));
            }
        }
        result.push(path.last().unwrap().0.clone());
        result.join(" → ")
    } else {
        "unknown".to_string()
    }
}

fn main() {
    let args = Args::parse();

    let cache_res = load_cache();
    
    let cache = if args.refresh {
        println!("Refreshing cache from official MTR API...");
        let (network, id_map) = fetch_mtr_data().expect("Failed to fetch MTR data");
        match scrape(&id_map) {
            Ok(mut new_cache) => {
                new_cache.network = network;
                save_cache(&new_cache).expect("Failed to save cache");
                new_cache
            }
            Err(e) => {
                eprintln!("Error scraping: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match cache_res {
            Ok(mut c) => {
                if c.network.is_empty() {
                    let (network, _) = fetch_mtr_data().expect("Failed to fetch MTR data");
                    c.network = network;
                    if !c.network.is_empty() {
                        save_cache(&c).expect("Failed to save cache");
                    }
                }
                c
            }
            Err(_) => {
                println!("Cache empty, refreshing from official MTR API...");
                let (network, id_map) = fetch_mtr_data().expect("Failed to fetch MTR data");
                match scrape(&id_map) {
                    Ok(mut new_cache) => {
                        new_cache.network = network;
                        save_cache(&new_cache).expect("Failed to save cache");
                        new_cache
                    }
                    Err(e) => {
                        eprintln!("Error scraping: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    };

    if args.refresh {
        return;
    }

    if args.matrix {
        print!("\t");
        println!("{}", cache.stations.join("\t"));
        for from in &cache.stations {
            print!("{}\t", from);
            let row = cache.matrix.get(from).cloned().unwrap_or_default();
            let values: Vec<String> = cache
                .stations
                .iter()
                .map(|to| row.get(to).map(|t| t.to_string()).unwrap_or_else(|| "".to_string()))
                .collect();
            println!("{}", values.join("\t"));
        }
        return;
    }

    if let (Some(from_query), Some(to_query)) = (args.from, args.to) {
        let from_station = match fuzzy_match(&from_query, &cache.stations) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };

        let to_station = match fuzzy_match(&to_query, &cache.stations) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };

        if let Some(row) = cache.matrix.get(from_station) {
            if let Some(time) = row.get(to_station) {
                println!("{} min", time);
                let route = find_route(from_station, to_station, &cache.stations, &cache.network);
                println!("Route: {}", route);
            } else {
                eprintln!("No journey time found between {} and {}", from_station, to_station);
                std::process::exit(1);
            }
        } else {
            eprintln!("No journey time data for {}", from_station);
            std::process::exit(1);
        }
    } else if !args.refresh && !args.matrix {
        eprintln!("Usage: poros <FROM> <TO> or poros --refresh or poros --matrix");
        std::process::exit(1);
    }
}
