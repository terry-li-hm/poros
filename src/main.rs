use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
#[command(author, version, about = "Queries MTR (Hong Kong subway) point-to-point journey times.")]
struct Args {
    /// Departure station (fuzzy matching, case-insensitive)
    #[arg(value_name = "FROM")]
    from: Option<String>,

    /// Destination station (fuzzy matching, case-insensitive)
    #[arg(value_name = "TO")]
    to: Option<String>,

    /// Re-scrapes piliapp and updates cache
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
}

fn get_cache_path() -> PathBuf {
    let mut path = home::home_dir().expect("Could not find home directory");
    path.push(".local/share/poros/cache.json");
    path
}

fn check_agent_browser() -> Result<(), String> {
    Command::new("agent-browser")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "agent-browser not found — install it first".to_string())?;
    Ok(())
}

fn run_agent_command(args: &[&str]) -> Result<String, String> {
    let output = Command::new("agent-browser")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run agent-browser: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "agent-browser failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn scrape() -> Result<Cache, String> {
    check_agent_browser()?;

    println!("Scraping piliapp.com (takes ~5 min)...");
    
    run_agent_command(&["open", "https://www.piliapp.com/hongkong-mtr/"])?;
    
    // Switch to Journey Time mode
    run_agent_command(&[
        "eval",
        "document.querySelectorAll('option')[0].selected=true; document.querySelectorAll('select')[0].dispatchEvent(new Event('change',{bubbles:true})); 'ok'"
    ])?;

    let mut cache = Cache::default();
    let mut station_names = Vec::new();

    for i in 1..=120 {
        let id = format!("t{}", i);
        let click_res = run_agent_command(&[
            "eval",
            &format!("var el=document.getElementById('{}'); if(el) {{ el.click(); 'ok' }} else 'skip'", id)
        ])?;

        if click_res.contains("skip") {
            continue;
        }

        print!("\rProgress: station {}/120...", i);
        io::stdout().flush().unwrap();

        run_agent_command(&["wait", "300"])?;

        let times_json = run_agent_command(&[
            "eval",
            "var r={}; document.querySelectorAll('span[id^=\"t\"]').forEach(function(s){ var name=s.textContent.trim(); var sib=s.nextSibling; var t=sib?sib.textContent.trim():''; if(t&&!isNaN(parseInt(t)))r[name]=parseInt(t); }); JSON.stringify(r)"
        ])?;

        // Handle possible quotes around the JSON string from agent-browser eval
        let times_json_clean = if (times_json.starts_with('"') && times_json.ends_with('"')) || (times_json.starts_with('\'') && times_json.ends_with('\'')) {
            &times_json[1..times_json.len()-1]
        } else {
            &times_json
        };
        
        // agent-browser might escape quotes in the JSON string
        let times_json_unescaped = times_json_clean.replace("\\\"", "\"");

        let times: HashMap<String, u32> = serde_json::from_str(&times_json_unescaped)
            .map_err(|e| format!("Failed to parse times JSON for {}: {}. JSON: {}", id, e, times_json_unescaped))?;

        let active_name = run_agent_command(&[
            "eval",
            &format!("document.getElementById('{}').textContent.trim()", id)
        ])?;
        
        let active_name = if (active_name.starts_with('"') && active_name.ends_with('"')) || (active_name.starts_with('\'') && active_name.ends_with('\'')) {
            active_name[1..active_name.len()-1].to_string()
        } else {
            active_name
        };

        if !station_names.contains(&active_name) {
            station_names.push(active_name.clone());
        }

        cache.matrix.insert(active_name, times);
    }

    println!("\nScraping complete.");
    run_agent_command(&["close"])?;

    station_names.sort();
    cache.stations = station_names;

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

fn mtr_network() -> Vec<(String, Vec<String>)> {
    vec![
        ("Island Line".to_string(), vec![
            "Kennedy Town".to_string(), "HKU".to_string(), "Sai Ying Pun".to_string(), "Sheung Wan".to_string(), "Central".to_string(), "Admiralty".to_string(), "Wan Chai".to_string(), "Causeway Bay".to_string(), "Tin Hau".to_string(), "Fortress Hill".to_string(), "North Point".to_string(), "Quarry Bay".to_string(), "Tai Koo".to_string(), "Sai Wan Ho".to_string(), "Shau Kei Wan".to_string(), "Heng Fa Chuen".to_string(), "Chai Wan".to_string()
        ]),
        ("Tsuen Wan Line".to_string(), vec![
            "Central".to_string(), "Admiralty".to_string(), "Tsim Sha Tsui".to_string(), "Jordan".to_string(), "Yau Ma Tei".to_string(), "Mong Kok".to_string(), "Prince Edward".to_string(), "Sham Shui Po".to_string(), "Cheung Sha Wan".to_string(), "Lai Chi Kok".to_string(), "Mei Foo".to_string(), "Lai King".to_string(), "Kwai Fong".to_string(), "Kwai Hing".to_string(), "Tai Wo Hau".to_string(), "Tsuen Wan".to_string()
        ]),
        ("Kwun Tong Line".to_string(), vec![
            "Whampoa".to_string(), "Ho Man Tin".to_string(), "Yau Ma Tei".to_string(), "Mong Kok".to_string(), "Prince Edward".to_string(), "Shek Kip Mei".to_string(), "Kowloon Tong".to_string(), "Lok Fu".to_string(), "Wong Tai Sin".to_string(), "Diamond Hill".to_string(), "Choi Hung".to_string(), "Kowloon Bay".to_string(), "Ngau Tau Kok".to_string(), "Kwun Tong".to_string(), "Lam Tin".to_string(), "Yau Tong".to_string(), "Tiu Keng Leng".to_string()
        ]),
        ("Tuen Ma Line".to_string(), vec![
            "Tuen Mun".to_string(), "Siu Hong".to_string(), "Tin Shui Wai".to_string(), "Long Ping".to_string(), "Yuen Long".to_string(), "Kam Sheung Road".to_string(), "Tsuen Wan West".to_string(), "Mei Foo".to_string(), "Nam Cheong".to_string(), "Austin".to_string(), "East Tsim Sha Tsui".to_string(), "Hung Hom".to_string(), "Ho Man Tin".to_string(), "To Kwa Wan".to_string(), "Sung Wong Toi".to_string(), "Kai Tak".to_string(), "Diamond Hill".to_string(), "Hin Keng".to_string(), "Tai Wai".to_string(), "Che Kung Temple".to_string(), "Sha Tin Wai".to_string(), "City One".to_string(), "Shek Mun".to_string(), "Tai Shui Hang".to_string(), "Heng On".to_string(), "Ma On shan".to_string(), "Wu Kai Sha".to_string()
        ]),
        ("East Rail Line".to_string(), vec![
            "Admiralty".to_string(), "Exhibition Centre".to_string(), "Hung Hom".to_string(), "Mong Kok East".to_string(), "Kowloon Tong".to_string(), "Tai Wai".to_string(), "Sha Tin".to_string(), "Fo Tan".to_string(), "University".to_string(), "Tai Po Market".to_string(), "Tai Wo".to_string(), "Fanling".to_string(), "Sheung Shui".to_string(), "Lo Wu".to_string()
        ]),
        ("East Rail Line (Lok Ma Chau)".to_string(), vec!["Sheung Shui".to_string(), "Lok Ma Chau".to_string()]),
        ("East Rail Line (Racecourse)".to_string(), vec!["Tai Wai".to_string(), "Racecourse".to_string(), "University".to_string()]),
        ("Tung Chung Line".to_string(), vec![
            "Hong Kong".to_string(), "Kowloon".to_string(), "Olympic".to_string(), "Nam Cheong".to_string(), "Lai King".to_string(), "Tsing Yi".to_string(), "Sunny Bay".to_string(), "Tung Chung".to_string()
        ]),
        ("Tseung Kwan O Line".to_string(), vec![
            "North Point".to_string(), "Quarry Bay".to_string(), "Yau Tong".to_string(), "Tiu Keng Leng".to_string(), "Tseung Kwan O".to_string(), "Hang Hau".to_string(), "Po Lam".to_string()
        ]),
        ("Tseung Kwan O Line (Lohas Park)".to_string(), vec!["Tseung Kwan O".to_string(), "Lohas Park".to_string()]),
        ("South Island Line".to_string(), vec![
            "Admiralty".to_string(), "Ocean Park".to_string(), "Wong Chuk Hang".to_string(), "Lei Tung".to_string(), "South Horizons".to_string()
        ]),
        ("Disneyland Resort Line".to_string(), vec![
            "Sunny Bay".to_string(), "Disneyland Resort".to_string()
        ]),
        ("Airport Express".to_string(), vec![
            "Hong Kong".to_string(), "Kowloon".to_string(), "Tsing Yi".to_string(), "Airport".to_string(), "AsiaWorld-Expo".to_string()
        ]),
    ]
}

fn find_route(from: &str, to: &str, cache_stations: &[String]) -> String {
    let network = mtr_network();
    let mut norm_to_canonical = HashMap::new();
    for s in cache_stations {
        norm_to_canonical.insert(normalize(s), s.clone());
    }

    let mut graph: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (line_name, stations) in &network {
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

    for (line_name, stations) in &network {
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
        for (line_name, stations) in &network {
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
        match scrape() {
            Ok(new_cache) => {
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
            Ok(c) => c,
            Err(_) => {
                println!("Cache empty, scraping piliapp.com (takes ~5 min)...");
                match scrape() {
                    Ok(new_cache) => {
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
                let route = find_route(from_station, to_station, &cache.stations);
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
