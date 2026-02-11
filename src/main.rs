use chrono::{Local, NaiveDate, NaiveTime, Timelike};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

// Constants for better maintainability
const DEADLINE_COOLDOWN: i64 = 3600; // 1 hour
const POPUP_DISPLAY_DURATION: i64 = 10; // 10 seconds
const CHECK_INTERVAL_SECS: u64 = 5; // 5 seconds
const POPUP_TIMEOUT: &str = "--timeout=10";

/// Structure to store a task
#[derive(Clone)]
struct Task {
    start: NaiveTime,
    end: NaiveTime,
    description: String,
    completed: bool,
    started: bool,
    completed_asked: bool,
    reason: Option<String>,
    // Pre-calculated for performance
    start_seconds: u32,
    end_seconds: u32,
}

impl Task {
    fn new(start: NaiveTime, end: NaiveTime, description: String) -> Self {
        Task {
            start,
            end,
            description,
            completed: false,
            started: false,
            completed_asked: false,
            reason: None,
            start_seconds: start.num_seconds_from_midnight(),
            end_seconds: end.num_seconds_from_midnight(),
        }
    }
}

/// Environment detection
struct Environment {
    has_zenity: bool,
    has_paplay: bool,
    is_headless: bool,
}

impl Environment {
    fn detect() -> Self {
        let has_zenity = Command::new("which")
            .arg("zenity")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);

        let has_paplay = Command::new("which")
            .arg("paplay")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);

        let is_headless = std::env::var("DISPLAY").is_err() && 
                         std::env::var("WAYLAND_DISPLAY").is_err();

        Environment {
            has_zenity: has_zenity && !is_headless,
            has_paplay,
            is_headless,
        }
    }
}

fn main() {
    clear_terminal();
    println!("📌 RemindR - Daily Task Reminder");
    println!("==================================\n");

    // Detect environment capabilities
    let env = Environment::detect();

    // Read reminders.txt
    let schedule_content = match fs::read_to_string("reminders.txt") {
        Ok(content) => content,
        Err(e) => {
            eprintln!("❌ Error: Could not read 'reminders.txt': {}", e);
            eprintln!("\nCreate 'reminders.txt' with lines like:");
            eprintln!("06:00-07:00 Study physics");
            eprintln!("07:30-08:00 Workout");
            eprintln!("DEADLINE 2026-02-28 Midterm Exam\n");
            std::process::exit(1);
        }
    };

    // Parse tasks and deadlines
    let mut tasks: Vec<Task> = Vec::new();
    let mut deadlines: Vec<(NaiveDate, String)> = Vec::new();

    for (_line_num, line) in schedule_content.lines().enumerate() {
        let line = line.trim();
        
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.to_uppercase().starts_with("DEADLINE ") {
            let parts: Vec<&str> = line.splitn(3, ' ').collect();
            if parts.len() >= 3 {
                if let Ok(date) = NaiveDate::parse_from_str(parts[1], "%Y-%m-%d") {
                    deadlines.push((date, parts[2].to_string()));
                }
            }
        } else {
            if let Some(space_pos) = line.find(' ') {
                let time_range = &line[..space_pos];
                let description = &line[space_pos + 1..];

                if let Some(dash_pos) = time_range.find('-') {
                    let start_s = &time_range[..dash_pos];
                    let end_s = &time_range[dash_pos + 1..];

                    if let (Ok(start), Ok(end)) = (
                        NaiveTime::parse_from_str(start_s, "%H:%M"),
                        NaiveTime::parse_from_str(end_s, "%H:%M"),
                    ) {
                        if start < end {
                            tasks.push(Task::new(start, end, description.to_string()));
                        }
                    }
                }
            }
        }
    }

    if tasks.is_empty() {
        eprintln!("❌ No valid tasks found in reminders.txt!");
        eprintln!("   Make sure tasks are in format: HH:MM-HH:MM Description");
        std::process::exit(1);
    }

    // Sort tasks by start time
    tasks.sort_by_key(|t| t.start);

    // Display today's schedule
    display_schedule(&tasks, &deadlines);
    
    // Track last shown time for deadlines
    let mut last_deadline_shown: HashMap<String, i64> = HashMap::new();
    let now_ts = Local::now().timestamp();
    
    for (_, desc) in &deadlines {
        last_deadline_shown.insert(desc.clone(), now_ts);
    }

    println!("⏰ Monitoring started. Running in background...");
    println!("Press Ctrl+C to stop RemindR\n");
    
    let mut pending_deadline_popups: HashMap<String, i64> = HashMap::new();
    let mut last_task_start_popup = String::new();
    
    // Setup Ctrl+C handler
    setup_ctrlc_handler();
    
    loop {
        let now = Local::now().time();
        let now_seconds = now.num_seconds_from_midnight();
        let now_ts = Local::now().timestamp();
        let today_now = Local::now().date_naive();

        // Check deadlines
        check_and_show_deadlines(
            &deadlines,
            &env,
            &mut last_deadline_shown,
            &mut pending_deadline_popups,
            now_ts,
            today_now,
        );

        // Clean up old pending popups
        pending_deadline_popups.retain(|_, ts| now_ts - *ts < POPUP_DISPLAY_DURATION);

        // Check each task
        for task in &mut tasks {
            // Task should start now - show popup and play alarm
            if now_seconds >= task.start_seconds && 
               now_seconds < task.end_seconds && 
               !task.started {
                task.started = true;
                
                if last_task_start_popup != task.description {
                    show_task_popup(&env, &format!("⏰ Task Starting:\n{}", task.description));
                    play_alarm(&env);
                    last_task_start_popup = task.description.clone();
                }
            }
            
            // Task just ended - ask if completed
            if task.started && 
               !task.completed && 
               !task.completed_asked && 
               now_seconds >= task.end_seconds {
                task.completed_asked = true;
                
                handle_task_completion(task, &env);
            }
        }

        // Check if all tasks passed and we can exit
        if should_exit(&tasks, now) {
            break;
        }

        thread::sleep(Duration::from_secs(CHECK_INTERVAL_SECS));
    }

    // End of day: save report
    if let Err(e) = write_daily_report(&tasks) {
        eprintln!("❌ Failed to write report: {}", e);
    }

    println!("\n✅ RemindR ended. Have a great day!\n");
}

/// Display today's schedule and deadlines
fn display_schedule(tasks: &[Task], deadlines: &[(NaiveDate, String)]) {
    println!("📅 Today's Schedule:");
    println!("─────────────────────────────────────");
    for t in tasks {
        println!("  {}-{} {}", 
            t.start.format("%H:%M"), 
            t.end.format("%H:%M"), 
            t.description
        );
    }
    println!();

    println!("📆 Upcoming Deadlines:");
    println!("─────────────────────────────────────");
    let today = Local::now().date_naive();
    
    if !deadlines.is_empty() {
        for (date, desc) in deadlines {
            let days_left = (*date - today).num_days();
            if days_left >= 0 {
                println!("  ⏳ {} (in {} days)", desc, days_left);
            } else {
                println!("  ⏳ {} ({} days ago!)", desc, days_left.abs());
            }
        }
    } else {
        println!("  (No deadlines)");
    }
    println!();
}

/// Handle task completion dialog
fn handle_task_completion(task: &mut Task, env: &Environment) {
    // Ask completion with YES/NO buttons
    let completed = ask_yes_no(&format!("Did you complete: {}", task.description), env);
    
    if completed {
        show_task_popup(env, "Great! One step closer to your goal 🎉");
        play_alarm(env);
        task.completed = true;
    } else {
        // Ask for reason
        task.reason = ask_reason(&format!("Why was '{}' not completed?", task.description), env);
        task.completed = false;
    }
}

/// Check if program should exit (all tasks completed or passed)
fn should_exit(tasks: &[Task], current_time: NaiveTime) -> bool {
    let all_tasks_passed = tasks.iter().all(|t| 
        current_time.num_seconds_from_midnight() >= t.end_seconds
    );
    
    if all_tasks_passed {
        if let Some(latest_end) = tasks.iter().map(|t| t.end).max() {
            return current_time > latest_end;
        }
    }
    
    false
}

/// Check deadlines and show popups if needed
fn check_and_show_deadlines(
    deadlines: &[(NaiveDate, String)],
    env: &Environment,
    last_shown: &mut HashMap<String, i64>,
    pending: &mut HashMap<String, i64>,
    now_ts: i64,
    today: NaiveDate,
) {
    for (date, desc) in deadlines {
        let days_left = (*date - today).num_days();
        
        let show = match last_shown.get(desc) {
            Some(&ts) => now_ts - ts >= DEADLINE_COOLDOWN,
            None => true,
        };

        if show && !pending.contains_key(desc) {
            let message = if days_left >= 0 {
                format!("⏳ Deadline: {}\n({} days left)", desc, days_left)
            } else {
                format!("⏳ Deadline: {}\n({} days overdue!)", desc, days_left.abs())
            };
            
            show_task_popup(env, &message);
            play_alarm(env);
            last_shown.insert(desc.clone(), now_ts);
            pending.insert(desc.clone(), now_ts);
        }
    }
}

/// Setup Ctrl+C handler for graceful shutdown
fn setup_ctrlc_handler() {
    ctrlc::set_handler(move || {
        println!("\n\n⚠️  RemindR interrupted by user");
        println!("📝 Daily report will still be saved.\n");
        // The program will exit after this handler
    }).expect("Error setting Ctrl-C handler");
}

/// Show task popup (non-blocking, non-freezing)
fn show_task_popup(env: &Environment, message: &str) {
    if env.has_zenity && !env.is_headless {
        let _ = Command::new("zenity")
            .arg("--info")
            .arg("--text")
            .arg(message)
            .arg(POPUP_TIMEOUT)
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn();
    }
}

/// Play alarm sound (non-blocking)
fn play_alarm(env: &Environment) {
    if env.has_paplay {
        let sound_paths = [
            "/usr/share/sounds/freedesktop/stereo/complete.oga",
            "/usr/share/sounds/freedesktop/stereo/bell.oga",
            "/usr/share/sounds/ubuntu/stereo/bells.oga",
            "/usr/share/sounds/freedesktop/stereo/alarm-clock-elapsed.oga",
        ];

        for sound_path in sound_paths {
            if std::path::Path::new(sound_path).exists() {
                let _ = Command::new("paplay")
                    .arg(sound_path)
                    .stderr(Stdio::null())
                    .stdout(Stdio::null())
                    .spawn();
                return;
            }
        }
        
        // Try fallback beep command
        let _ = Command::new("beep")
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn();
    }
    
    // Terminal bell as final fallback
    print!("\x07");
    let _ = io::stdout().flush();
}

/// Ask Yes/No question with blocking dialog
fn ask_yes_no(question: &str, env: &Environment) -> bool {
    if env.has_zenity && !env.is_headless {
        if let Ok(status) = Command::new("zenity")
            .arg("--question")
            .arg("--text")
            .arg(question)
            .arg("--ok-label=Yes")
            .arg("--cancel-label=No")
            .stderr(Stdio::null())
            .status()
        {
            return status.success();
        }
    }

    // Fallback to terminal
    println!("\n{} (y/n): ", question);
    let _ = io::stdout().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let ans = input.trim().to_lowercase();
        return ans == "y" || ans == "yes";
    }
    false
}

/// Ask reason for not completing a task (blocking dialog)
fn ask_reason(question: &str, env: &Environment) -> Option<String> {
    if env.has_zenity && !env.is_headless {
        if let Ok(output) = Command::new("zenity")
            .arg("--entry")
            .arg("--text")
            .arg(question)
            .arg("--title")
            .arg("Task Incomplete Reason")
            .stderr(Stdio::null())
            .output()
        {
            let reason = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !reason.is_empty() {
                return Some(reason);
            }
        }
    }

    // Fallback to terminal
    println!("\n{}", question);
    let _ = io::stdout().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let reason = input.trim().to_string();
        if !reason.is_empty() {
            return Some(reason);
        }
    }

    None
}

/// Write daily report with completion status
fn write_daily_report(tasks: &[Task]) -> Result<(), std::io::Error> {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let filename = format!("daily_report_{}.txt", date);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&filename)?;

    writeln!(file, "📌 RemindR Daily Report")?;
    writeln!(file, "Date: {}\n", date)?;
    writeln!(file, "Tasks Summary")?;
    writeln!(file, "{}", "=".repeat(70))?;
    writeln!(file)?;

    let mut completed_count = 0;
    let mut incomplete_count = 0;

    for t in tasks {
        if t.completed {
            writeln!(file, "✅ COMPLETED")?;
            completed_count += 1;
        } else if t.started {
            writeln!(file, "❌ NOT COMPLETED")?;
            incomplete_count += 1;
        } else {
            writeln!(file, "⏭️  SKIPPED")?;
            incomplete_count += 1;
        }
        
        writeln!(file, "   Time: {}-{}", 
            t.start.format("%H:%M"), 
            t.end.format("%H:%M")
        )?;
        writeln!(file, "   Task: {}", t.description)?;
        
        if let Some(reason) = &t.reason {
            writeln!(file, "   Reason: {}", reason)?;
        }
        
        writeln!(file)?;
    }

    writeln!(file, "{}", "=".repeat(70))?;
    writeln!(file)?;
    writeln!(file, "Summary:")?;
    writeln!(file, "  Total Tasks: {}", tasks.len())?;
    writeln!(file, "  ✅ Completed: {}", completed_count)?;
    writeln!(file, "  ❌ Not Completed: {}", incomplete_count)?;
    
    let percentage = if !tasks.is_empty() {
        (completed_count as f64 / tasks.len() as f64) * 100.0
    } else {
        0.0
    };
    writeln!(file, "  Completion Rate: {:.0}%", percentage)?;
    writeln!(file)?;
    writeln!(file, "Generated: {}", Local::now().format("%Y-%m-%d %H:%M:%S"))?;
    
    println!("📊 Daily report saved to: {}", filename);
    Ok(())
}

/// Clear terminal
fn clear_terminal() {
    print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
}
