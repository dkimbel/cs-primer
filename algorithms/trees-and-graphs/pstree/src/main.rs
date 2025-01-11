use crossterm::style::{style, Color, Stylize};
use crossterm::terminal;
use regex::Regex;
use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::sync::OnceLock;

// Minor optimization to only compile our regex one time throughout life of program
static PROCESS_LINE_REGEX: OnceLock<Regex> = OnceLock::new();
// These are the colors our tree characters will cycle through as they become
// increasingly nested.
const COLORS: [Color; 3] = [Color::Yellow, Color::Red, Color::Cyan];
const ROOT_PARENT_PID: usize = 0;

/// Chars that will be printed to the screen to reflect the structure of the tree.
/// R = Right, L = Left, T = Top, B = Bottom. So e.g. RL is a dash-like char that
/// extends from left to right.
enum TreeChar {
    RL,
    RBL,
    TRB,
    TB,
    TR,
}

impl TreeChar {
    fn to_char(&self) -> char {
        use TreeChar::*;
        match self {
            RL => '─',
            RBL => '┬',
            TRB => '├',
            TB => '│',
            TR => '└',
        }
    }
}

// Any given Process's ChildPosition is that process's position relative to its own immediate
// parent. So if Process id 10 is the third of four children of Process id 2, then Process id
// 10 is a MiddleChild. Process id 10 may or may not have children of its own -- that isn't
// relevant here.
#[derive(Clone, Copy, PartialEq)]
enum ChildPosition {
    MiddleChild, // includes first child
    LastChild,
}

// Formulated to "make illegal state unrepresentable" -- there is no way to represent any
// given Process as not being a child, yet having parents.
#[derive(Clone, Copy)]
enum ChildStatus<'a> {
    NotChild,
    IsChild {
        position: ChildPosition,
        // if this is empty, then the relevant process only has one parent: the root.
        non_root_parent_child_positions: &'a Vec<ChildPosition>,
    },
}

#[derive(Clone, PartialEq)]
struct Process {
    pid: usize,
    user: String,
    args: String,
}

impl Process {
    /// Create a Process from a line outputted by a `ps` command using a particular format.
    /// Return a tuple of the new Process and its parent PID. (Keeping the parent PID
    /// separate from the struct is just a slight optimization to avoid storing extra copies
    /// of it unnecessarily, when we put the struct into a map with parent PID as the key.)
    fn from_ps_line(line: &str) -> (Self, usize) {
        let re = PROCESS_LINE_REGEX
            // example line: "  391     1 root             /usr/libexec/keybagd -t 15"
            .get_or_init(|| Regex::new(r"^\s*(\d+)\s+(\d+)\s+(\w+)\s+(.*?)\s*$").unwrap());
        let captures = re
            .captures(line)
            .expect(&format!("Failed to parse line from ps: {line}"));

        let parent_pid = captures[2]
            .parse::<usize>()
            .expect("failed to parse ppid as integer");

        (
            Self {
                pid: captures[1]
                    .parse::<usize>()
                    .expect("failed to parse pid as integer"),
                user: captures[3].to_string(),
                args: captures[4].to_string(),
            },
            parent_pid,
        )
    }

    /// Filter processes by the given text (case-insensitive). Matching processes _and all of
    /// their parents_ will be copied from the `all` map to the `filtered` map.
    fn filter_by_text_recursive<'proc>(
        &self,
        uppercased_filter_text: &str,
        parents: &Vec<&Process>,
        all_parent_pids_to_child_processes: &HashMap<usize, Vec<Process>>,
        filtered_parent_pids_to_child_processes: &mut HashMap<usize, Vec<Process>>,
    ) {
        if self.args.to_uppercase().contains(uppercased_filter_text) {
            // This process matches our filter! Merge it and its parents into our filtered map.
            // Note: since I left parent pid out of the process struct, we need to keep track of
            // it ourselves.
            let mut parent_pid = ROOT_PARENT_PID; // the first parent's parent is always root
            for process in parents.iter().chain(std::iter::once(&self)) {
                let entry = filtered_parent_pids_to_child_processes
                    .entry(parent_pid)
                    .or_insert_with(Vec::new);
                // This `contains` check runs in O(n) time (not great), but these lists shouldn't be
                // very long. If necessary, we could start to use a HashSet for O(1) lookup, and later
                // convert the set to a list (sorted by process ID!).
                if !entry.contains(process) {
                    entry.push((*process).clone());
                }
                parent_pid = process.pid;
            }
        }

        // Recurse through all children of this process
        if let Some(children) = all_parent_pids_to_child_processes.get(&self.pid) {
            let mut childs_parents = parents.clone();
            childs_parents.push(self);

            for child in children {
                Self::filter_by_text_recursive(
                    child,
                    uppercased_filter_text,
                    &childs_parents,
                    all_parent_pids_to_child_processes,
                    filtered_parent_pids_to_child_processes,
                )
            }
        }
    }

    fn print_recursive(
        &self,
        max_num_pid_chars: usize,
        terminal_width: usize,
        child_status: ChildStatus,
        parent_pids_to_child_processes: &HashMap<usize, Vec<Process>>,
    ) {
        let maybe_children = parent_pids_to_child_processes.get(&self.pid);
        let is_parent = maybe_children.is_some_and(|children| !children.is_empty());

        // Do the actual printing
        let (tree_chars, num_visible_tree_chars) = self.get_tree_chars(is_parent, child_status);
        let Self { pid, user, args } = self;
        let formatted_pid = format!("{pid:0max_num_pid_chars$}");
        // We add 3 to deal with whitespace we added between different pieces of text. Meanwhile,
        // we assume text will be ASCII and so use len() instead of .chars().count().
        let visible_content_length =
            num_visible_tree_chars + 3 + formatted_pid.len() + user.len() + args.len();
        let process_line = format!(
            "{tree_chars} {} {} {args}",
            formatted_pid.blue(),
            style(&user).with(Color::Magenta)
        );
        // We have to calculate how many ansi color code characters are present, because we add
        // a variable amount of them to our tree chars.
        let num_ansi_color_chars = process_line.chars().count() - visible_content_length;
        let num_chars_to_print = terminal_width + num_ansi_color_chars;
        println!("{process_line:.num_chars_to_print$}");

        // recursively print all children of the current process
        if let Some(children) = maybe_children {
            // Each process must know the 'child position' of ALL of its parents, to know
            // whether to print whitespace/indentation (parent was a last child) or a
            // top-to-bottom char (parent was a middle child, and so has more processes
            // below it at the same indentation level). To support this need, maintain
            // a running list of parent 'child positions'.
            let non_root_parent_child_positions: Vec<ChildPosition> = match child_status {
                ChildStatus::NotChild => Vec::new(),
                ChildStatus::IsChild {
                    position,
                    non_root_parent_child_positions,
                } => {
                    let mut v = non_root_parent_child_positions.clone();
                    v.push(position);
                    v
                }
            };

            for (i, child_process) in children.iter().enumerate() {
                // The child needs to know whether it is itself a 'middle' or 'last' child;
                // this affects the tree chars it prints.
                let childs_child_position = if i + 1 == children.len() {
                    ChildPosition::LastChild
                } else {
                    ChildPosition::MiddleChild
                };
                let childs_child_status = ChildStatus::IsChild {
                    position: childs_child_position,
                    non_root_parent_child_positions: &non_root_parent_child_positions,
                };
                Self::print_recursive(
                    child_process,
                    max_num_pid_chars,
                    terminal_width,
                    childs_child_status,
                    parent_pids_to_child_processes,
                );
            }
        }
    }

    fn get_tree_chars(&self, is_parent: bool, child_status: ChildStatus) -> (String, usize) {
        match child_status {
            ChildStatus::NotChild => {
                let middle_tree_char = if is_parent {
                    TreeChar::RBL
                } else {
                    TreeChar::RL
                };
                let s = [TreeChar::RL, middle_tree_char, TreeChar::RL]
                    .iter()
                    .map(|tc| tc.to_char())
                    .collect::<String>();
                let styled = style(&s).with(COLORS[0]).to_string();
                (styled, s.len())
            }
            ChildStatus::IsChild {
                position,
                non_root_parent_child_positions,
            } => {
                let mut s = String::from(' ');
                let mut num_uncolored_chars: usize = 1;
                let mut colors_i: usize = 0;

                // We need to indent this Process further based on how many parents
                // it has. We might also need to draw some top-to-bottom lines on
                // behalf of those parents, to reach their further-down siblings.
                for parent_child_position in non_root_parent_child_positions {
                    let position_char = match parent_child_position {
                        ChildPosition::MiddleChild => TreeChar::TB.to_char(),
                        ChildPosition::LastChild => ' ',
                    };
                    let mut unstyled = String::new();
                    unstyled.push(position_char);
                    unstyled.push(' ');
                    num_uncolored_chars += 2;
                    let styled = style(unstyled)
                        .with(COLORS[colors_i % COLORS.len()])
                        .to_string();
                    s.extend(styled.chars());

                    colors_i += 1;
                }

                // add the final tree characters, which may look like the
                // following example (among others): └─┬─
                let position_tree_char = match position {
                    ChildPosition::MiddleChild => TreeChar::TRB,
                    ChildPosition::LastChild => TreeChar::TR,
                };
                let branch_to_children_tree_char = match is_parent {
                    true => TreeChar::RBL,
                    false => TreeChar::RL,
                };
                let final_chars = [
                    position_tree_char,
                    TreeChar::RL,
                    branch_to_children_tree_char,
                    TreeChar::RL,
                ]
                .map(|tc| tc.to_char())
                .iter()
                .collect::<String>();
                num_uncolored_chars += 4;
                let final_chars_styled = style(final_chars)
                    .with(COLORS[colors_i % COLORS.len()])
                    .to_string();
                s.extend(final_chars_styled.chars());
                (s, num_uncolored_chars)
            }
        }
    }
    // TODO Make matched text bolded! Without ruining full-width text display.
    // TODO any way to be polymorphic between reference and non-reference? So I can let my filtered
    //   tree just use references to processes, and not clone?
    // TODO Any refactor / code cleanup?
    //   - could I possibly have reusable 'tree search' code that takes some kind of 'action' as an
    //     input? that action could be 'print', or it could be 'check for text match and merge into tree'.
    //     - could I at least have a StackSearch struct that keeps the mutable stack as an attr, and so
    //       can easily have separate helper functions for 'handle parent pop' and 'handle print'? BUT,
    //       I don't know how well that would work in Rust... would I have to always have mutable refs
    //       on both lists from my helpers, preventing me from handing out an immutable ref to the
    //       process printing fn?
    //   - maybe have a ProcessPrinter that keeps track of max_num_process_chars for us, plus terminal
    //     width and even hashmap of parent PIDs to child processes? But, big question: how to share code
    //     between the ProcessPrinter and a ProcessSearcher used to filter for processes that match string?
    //   - can I clean up 'num ansi chars' calculation code?
    //   - try to be guided by common arguments not having to be passed to functions, because they belong
    //     to a relevant struct already?
    //   - generally split out parsing of the full process list into its own function / struct?
    //   - do I reasonably need to split into multiple files?
    //   - get rid of any/all remaining compilation warnings
    //   - add a comment on how I could have done printing in the same pass as parsing, since inputs
    //     were pre-sorted. But just as well to keep that separate given optional filtering step.
    // TODO add/update comments? add missing docstrings?
    // TODO Add README featuring a screenshot and noting crossterm. Also have instructions for doing a prod
    //   build and then calling the compiled executable.
    // TODO Submit, mentioning screenshot in README or direct-linking it; also note crossterm for width/colors
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() > 1 {
        panic!(
            "Only one argument is allowed; it will be used to filter the displayed processes. \
            To filter by a phrase containing whitespace, enclose the phrase in quotation marks."
        )
    }
    let filter_processes_by_text = args.get(0);

    let ps_stdout_bytes = Command::new("ps")
        .args(["-axo", "pid,ppid,user,args"])
        .output()
        .expect("ps command failed")
        .stdout;
    let ps_stdout = String::from_utf8(ps_stdout_bytes).expect("ps failed to output valid utf-8");

    // To model a tree (graph where every child can have only one parent), we use a map
    // of parent PID to process instance. We could do something more elaborate where each
    // Process owns a Vec<Process> of its children, but that isn't necessary.
    let mut all_parent_pids_to_child_processes: HashMap<usize, Vec<Process>> = HashMap::new();
    let mut max_pid = 0;

    // We use skip(1) to skip the first line, which just contains headers.
    for ps_line in ps_stdout.lines().skip(1) {
        let (process, parent_pid) = Process::from_ps_line(ps_line);
        // Technically we don't HAVE to call `max`, lines are already sorted by PID.
        max_pid = std::cmp::max(max_pid, process.pid);

        all_parent_pids_to_child_processes
            .entry(parent_pid)
            .or_insert_with(Vec::new)
            .push(process);
    }

    // We'll want to left-pad every printed PID with zeroes until it matches the length
    // of the largest PID.
    let max_num_pid_chars = format!("{max_pid}").len();

    let terminal_width = terminal::size()
        .expect("Failed to find terminal's dimensions")
        .0 as usize;

    // The root process will always be the only child of a special parent PID.
    let root_process_list: &Vec<Process> = &all_parent_pids_to_child_processes
        .get(&ROOT_PARENT_PID)
        .expect("A root process with parent pid 0 must exist");

    assert!(
        root_process_list.len() == 1,
        "There can only be one root process",
    );

    let all_processes_root = &root_process_list[0];

    // If we were given text to filter processes by, create a new filtered tree.
    let parent_pids_to_child_processes = if let Some(filter_text) = filter_processes_by_text {
        let mut filtered_parent_pids_to_child_processes = HashMap::new();
        let parents: Vec<&Process> = Vec::new();
        let uppercased_filter_text = filter_text.to_uppercase();

        Process::filter_by_text_recursive(
            all_processes_root,
            &uppercased_filter_text,
            &parents,
            &all_parent_pids_to_child_processes,
            &mut filtered_parent_pids_to_child_processes,
        );
        filtered_parent_pids_to_child_processes
    } else {
        all_parent_pids_to_child_processes
    };

    if let Some(root) = &parent_pids_to_child_processes
        .get(&ROOT_PARENT_PID)
        .and_then(|root_process_list| root_process_list.get(0))
    {
        // Print our tree of processes -- either all of them, or all that remain after
        // filtering. In principle we always want to do this, but it's nested in a conditional
        // because we could have filtered out _everything_. In that case there isn't even
        // a root to start recursing from.
        Process::print_recursive(
            root,
            max_num_pid_chars,
            terminal_width,
            ChildStatus::NotChild,
            &parent_pids_to_child_processes,
        );
    }
}
