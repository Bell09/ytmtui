//! One binding table for the keyboard, the dispatcher and the `?` cheatsheet.
//!
//! The event loop resolves a key to an [`Action`] here, and the cheatsheet is
//! rendered from the same [`BINDINGS`] slice. A key can therefore neither be
//! handled without appearing in the help nor listed without dispatching.

use crossterm::event::{KeyCode, KeyModifiers};

/// Which screen or modal owns the keyboard right now. Contexts are tried in
/// the order the event loop uses: global, help, text entry, pickers, normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Always checked before any screen, so Ctrl+C quits everywhere.
    Global,
    /// The `?` cheatsheet overlay.
    Help,
    /// `/` search query entry.
    Search,
    /// `c` playlist-name entry.
    CreatePlaylist,
    /// `a` add-to-playlist card.
    AddPicker,
    /// `b` browse-shelf card.
    BrowsePicker,
    /// Ordinary navigation.
    Normal,
}

/// Everything a key can do in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,

    // Cheatsheet
    OpenHelp,
    CloseHelp,
    HelpLineUp,
    HelpLineDown,
    HelpPageUp,
    HelpPageDown,
    HelpTop,
    HelpBottom,

    // Normal-mode navigation
    NextRow,
    PreviousRow,
    Activate,
    Back,
    OpenBrowseSection(usize),
    OpenBrowsePicker,
    FocusSearch,
    CycleSearchFilter(bool),
    ScrollPage(i16),
    ToggleOffline,

    // Playback
    TogglePlay,
    SeekBack,
    SeekForward,
    PlayNext,
    PlayPrevious,
    ToggleLike,
    ToggleLyrics,
    ToggleNowPlaying,
    ToggleAutoQueue,
    CycleRepeat,
    ToggleShuffle,

    // Queue
    OpenQueue,
    QueuePlayNext,
    QueueAppend,
    QueueMoveDown,
    QueueMoveUp,

    // Songs and playlists
    DownloadOrDelete,
    OpenAddPicker,
    StartCreatePlaylist,
    RemoveFromPlaylist,

    // Search text entry
    SearchSubmit,
    SearchCancel,
    SearchBackspace,
    SearchInsert,

    // Create-playlist text entry
    CreateSubmit,
    CreateCancel,
    CreateBackspace,
    CreateInsert,

    // Add-to-playlist picker
    AddPickerNext,
    AddPickerPrevious,
    AddPickerConfirm,
    AddPickerCancel,

    // Browse-shelf picker
    BrowsePickerNext,
    BrowsePickerPrevious,
    BrowsePickerConfirm,
    BrowsePickerCancel,
}

/// Sections shown in the cheatsheet, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Global,
    Navigation,
    Playback,
    Queue,
    Library,
    Search,
    CreatePlaylist,
    AddPicker,
    BrowsePicker,
    Help,
}

impl Category {
    const ALL: [Category; 10] = [
        Category::Global,
        Category::Navigation,
        Category::Playback,
        Category::Queue,
        Category::Library,
        Category::Search,
        Category::CreatePlaylist,
        Category::AddPicker,
        Category::BrowsePicker,
        Category::Help,
    ];

    fn index(self) -> usize {
        match self {
            Category::Global => 0,
            Category::Navigation => 1,
            Category::Playback => 2,
            Category::Queue => 3,
            Category::Library => 4,
            Category::Search => 5,
            Category::CreatePlaylist => 6,
            Category::AddPicker => 7,
            Category::BrowsePicker => 8,
            Category::Help => 9,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Category::Global => "Global",
            Category::Navigation => "Navigation",
            Category::Playback => "Playback",
            Category::Queue => "Queue",
            Category::Library => "Songs & playlists",
            Category::Search => "Search input",
            Category::CreatePlaylist => "Create playlist",
            Category::AddPicker => "Add to playlist picker",
            Category::BrowsePicker => "Browse shelf picker",
            Category::Help => "Cheatsheet",
        }
    }
}

/// How Ctrl must be set for a binding to match. Most keys ignore modifiers, as
/// the original dispatcher did; only keys with a Ctrl twin need a strict bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctrl {
    Any,
    Yes,
    No,
}

/// A key to match, or the "any character" wildcard used by text fields.
#[derive(Debug, Clone, Copy)]
enum Key {
    Code(KeyCode),
    AnyChar,
}

/// Help metadata attached to a binding. Grouped bindings share `group` so the
/// cheatsheet can print `↑ / ↓` on one row without a second key list.
#[derive(Debug, Clone, Copy)]
struct Help {
    category: Category,
    group: &'static str,
    description: &'static str,
}

impl Help {
    const fn new(category: Category, group: &'static str, description: &'static str) -> Self {
        Self {
            category,
            group,
            description,
        }
    }
}

/// A key definition. The same entry drives both input dispatch and the help
/// screen.
struct Binding {
    context: Context,
    key: Key,
    ctrl: Ctrl,
    action: Action,
    help: Option<Help>,
}

impl Binding {
    const fn new(
        context: Context,
        key: KeyCode,
        ctrl: Ctrl,
        action: Action,
        help: Option<Help>,
    ) -> Self {
        Self {
            context,
            key: Key::Code(key),
            ctrl,
            action,
            help,
        }
    }

    /// Any typed character. Used only by the two text-entry contexts.
    const fn any_char(context: Context, action: Action, help: Option<Help>) -> Self {
        Self {
            context,
            key: Key::AnyChar,
            ctrl: Ctrl::Any,
            action,
            help,
        }
    }

    fn matches(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let key_matches = match self.key {
            Key::Code(expected) => expected == code,
            Key::AnyChar => matches!(code, KeyCode::Char(_)),
        };
        if !key_matches {
            return false;
        }
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match self.ctrl {
            Ctrl::Any => true,
            Ctrl::Yes => ctrl,
            Ctrl::No => !ctrl,
        }
    }

    fn key_label(&self) -> String {
        let mut label = match self.key {
            Key::AnyChar => "type".to_string(),
            Key::Code(code) => match code {
                KeyCode::Char(' ') => "Space".to_string(),
                KeyCode::Char(c) => c.to_string(),
                KeyCode::Enter => "Enter".to_string(),
                KeyCode::Esc => "Esc".to_string(),
                KeyCode::Tab => "Tab".to_string(),
                KeyCode::BackTab => "Shift+Tab".to_string(),
                KeyCode::Up => "↑".to_string(),
                KeyCode::Down => "↓".to_string(),
                KeyCode::Left => "←".to_string(),
                KeyCode::Right => "→".to_string(),
                KeyCode::PageUp => "PgUp".to_string(),
                KeyCode::PageDown => "PgDn".to_string(),
                KeyCode::Home => "Home".to_string(),
                KeyCode::End => "End".to_string(),
                other => format!("{other:?}"),
            },
        };
        if self.ctrl == Ctrl::Yes {
            // Ctrl chords are conventionally written with a capital letter.
            if label.len() == 1 && label.as_bytes()[0].is_ascii_lowercase() {
                label.make_ascii_uppercase();
            }
            label.insert_str(0, "Ctrl+");
        }
        label
    }
}

const fn help(category: Category, group: &'static str, description: &'static str) -> Option<Help> {
    Some(Help::new(category, group, description))
}

const fn normal(
    key: KeyCode,
    ctrl: Ctrl,
    action: Action,
    category: Category,
    group: &'static str,
    description: &'static str,
) -> Binding {
    Binding::new(
        Context::Normal,
        key,
        ctrl,
        action,
        help(category, group, description),
    )
}

/// Every key the application understands, in cheatsheet order. Normal-mode
/// bindings come first; modal bindings follow, with wildcards last so Enter,
/// Esc and Backspace win over text insertion.
const BINDINGS: &[Binding] = &[
    // -- Global -------------------------------------------------------------
    Binding::new(
        Context::Global,
        KeyCode::Char('c'),
        Ctrl::Yes,
        Action::Quit,
        help(
            Category::Global,
            "global-quit",
            "Quit from any screen or text field",
        ),
    ),
    normal(
        KeyCode::Char('q'),
        Ctrl::Any,
        Action::Quit,
        Category::Global,
        "quit",
        "Quit ytmtui (normal mode)",
    ),
    normal(
        KeyCode::Char('?'),
        Ctrl::Any,
        Action::OpenHelp,
        Category::Global,
        "help",
        "Open this cheatsheet (normal mode)",
    ),
    // -- Navigation ---------------------------------------------------------
    normal(
        KeyCode::Up,
        Ctrl::Any,
        Action::PreviousRow,
        Category::Navigation,
        "move",
        "Move through the current list",
    ),
    normal(
        KeyCode::Down,
        Ctrl::Any,
        Action::NextRow,
        Category::Navigation,
        "move",
        "Move through the current list",
    ),
    normal(
        KeyCode::Enter,
        Ctrl::Any,
        Action::Activate,
        Category::Navigation,
        "activate",
        "Play or open the highlighted item",
    ),
    normal(
        KeyCode::Esc,
        Ctrl::Any,
        Action::Back,
        Category::Navigation,
        "back",
        "Back: close Now Playing or leave this view",
    ),
    normal(
        KeyCode::Char('1'),
        Ctrl::Any,
        Action::OpenBrowseSection(0),
        Category::Navigation,
        "browse-section",
        "Open Browse section 1–5 directly",
    ),
    normal(
        KeyCode::Char('2'),
        Ctrl::Any,
        Action::OpenBrowseSection(1),
        Category::Navigation,
        "browse-section",
        "Open Browse section 1–5 directly",
    ),
    normal(
        KeyCode::Char('3'),
        Ctrl::Any,
        Action::OpenBrowseSection(2),
        Category::Navigation,
        "browse-section",
        "Open Browse section 1–5 directly",
    ),
    normal(
        KeyCode::Char('4'),
        Ctrl::Any,
        Action::OpenBrowseSection(3),
        Category::Navigation,
        "browse-section",
        "Open Browse section 1–5 directly",
    ),
    normal(
        KeyCode::Char('5'),
        Ctrl::Any,
        Action::OpenBrowseSection(4),
        Category::Navigation,
        "browse-section",
        "Open Browse section 1–5 directly",
    ),
    normal(
        KeyCode::Char('b'),
        Ctrl::Any,
        Action::OpenBrowsePicker,
        Category::Navigation,
        "browse-picker",
        "Open the Browse shelf picker",
    ),
    normal(
        KeyCode::Char('/'),
        Ctrl::Any,
        Action::FocusSearch,
        Category::Navigation,
        "search",
        "Focus the search bar",
    ),
    normal(
        KeyCode::Tab,
        Ctrl::Any,
        Action::CycleSearchFilter(true),
        Category::Navigation,
        "filter",
        "Cycle Search filters (Search page only)",
    ),
    normal(
        KeyCode::BackTab,
        Ctrl::Any,
        Action::CycleSearchFilter(false),
        Category::Navigation,
        "filter",
        "Cycle Search filters (Search page only)",
    ),
    normal(
        KeyCode::PageUp,
        Ctrl::Any,
        Action::ScrollPage(-1),
        Category::Navigation,
        "page-scroll",
        "Scroll lyrics, otherwise Library details",
    ),
    normal(
        KeyCode::PageDown,
        Ctrl::Any,
        Action::ScrollPage(1),
        Category::Navigation,
        "page-scroll",
        "Scroll lyrics, otherwise Library details",
    ),
    // -- Playback -----------------------------------------------------------
    normal(
        KeyCode::Char(' '),
        Ctrl::Any,
        Action::TogglePlay,
        Category::Playback,
        "play",
        "Play or pause",
    ),
    normal(
        KeyCode::Left,
        Ctrl::Any,
        Action::SeekBack,
        Category::Playback,
        "seek",
        "Seek back / forward 10 seconds",
    ),
    normal(
        KeyCode::Right,
        Ctrl::Any,
        Action::SeekForward,
        Category::Playback,
        "seek",
        "Seek back / forward 10 seconds",
    ),
    normal(
        KeyCode::Char('n'),
        Ctrl::Any,
        Action::PlayNext,
        Category::Playback,
        "skip",
        "Next / previous track",
    ),
    normal(
        KeyCode::Char('p'),
        Ctrl::Any,
        Action::PlayPrevious,
        Category::Playback,
        "skip",
        "Next / previous track",
    ),
    normal(
        KeyCode::Char('L'),
        Ctrl::Any,
        Action::ToggleLike,
        Category::Playback,
        "like",
        "Like/unlike playing song; Liked: selected",
    ),
    normal(
        KeyCode::Char('l'),
        Ctrl::Any,
        Action::ToggleLyrics,
        Category::Playback,
        "lyrics",
        "Show or hide the lyrics pane",
    ),
    normal(
        KeyCode::Char('m'),
        Ctrl::Any,
        Action::ToggleNowPlaying,
        Category::Playback,
        "maximize",
        "Maximize or restore Now Playing",
    ),
    normal(
        KeyCode::Char('a'),
        Ctrl::Yes,
        Action::ToggleAutoQueue,
        Category::Playback,
        "auto-queue",
        "Toggle Auto Queue",
    ),
    normal(
        KeyCode::Char('r'),
        Ctrl::Yes,
        Action::CycleRepeat,
        Category::Playback,
        "repeat",
        "Cycle repeat mode",
    ),
    normal(
        KeyCode::Char('s'),
        Ctrl::Yes,
        Action::ToggleShuffle,
        Category::Playback,
        "shuffle",
        "Toggle shuffle",
    ),
    // -- Queue --------------------------------------------------------------
    normal(
        KeyCode::Char('z'),
        Ctrl::Any,
        Action::OpenQueue,
        Category::Queue,
        "queue",
        "Open the queue (Esc returns)",
    ),
    normal(
        KeyCode::Char('X'),
        Ctrl::Any,
        Action::QueuePlayNext,
        Category::Queue,
        "queue-next",
        "Play next; on Queue, promote selected",
    ),
    normal(
        KeyCode::Char('Z'),
        Ctrl::Any,
        Action::QueueAppend,
        Category::Queue,
        "queue-append",
        "Append selected track to the queue",
    ),
    normal(
        KeyCode::Char('J'),
        Ctrl::Any,
        Action::QueueMoveDown,
        Category::Queue,
        "queue-move",
        "Move selected queue track down / up",
    ),
    normal(
        KeyCode::Char('K'),
        Ctrl::Any,
        Action::QueueMoveUp,
        Category::Queue,
        "queue-move",
        "Move selected queue track down / up",
    ),
    // -- Songs and playlists ------------------------------------------------
    normal(
        KeyCode::Char('d'),
        Ctrl::Any,
        Action::DownloadOrDelete,
        Category::Library,
        "download",
        "Download song; on Queue, remove selected",
    ),
    normal(
        KeyCode::Char('a'),
        Ctrl::No,
        Action::OpenAddPicker,
        Category::Library,
        "add",
        "Open Add to Playlist picker",
    ),
    normal(
        KeyCode::Char('A'),
        Ctrl::Any,
        Action::OpenAddPicker,
        Category::Library,
        "add",
        "Open Add to Playlist picker",
    ),
    normal(
        KeyCode::Char('c'),
        Ctrl::Any,
        Action::StartCreatePlaylist,
        Category::Library,
        "create",
        "Create a playlist",
    ),
    normal(
        KeyCode::Char('r'),
        Ctrl::No,
        Action::RemoveFromPlaylist,
        Category::Library,
        "remove",
        "Remove song from the open playlist",
    ),
    normal(
        KeyCode::Char('o'),
        Ctrl::Any,
        Action::ToggleOffline,
        Category::Library,
        "offline",
        "Toggle offline mode",
    ),
    // -- Search text entry --------------------------------------------------
    Binding::new(
        Context::Search,
        KeyCode::Enter,
        Ctrl::Any,
        Action::SearchSubmit,
        help(
            Category::Search,
            "search-submit",
            "Run the query and leave the search bar",
        ),
    ),
    Binding::new(
        Context::Search,
        KeyCode::Esc,
        Ctrl::Any,
        Action::SearchCancel,
        help(
            Category::Search,
            "search-cancel",
            "Cancel and clear the search",
        ),
    ),
    Binding::new(
        Context::Search,
        KeyCode::Backspace,
        Ctrl::Any,
        Action::SearchBackspace,
        help(
            Category::Search,
            "search-delete",
            "Delete the last character",
        ),
    ),
    Binding::any_char(
        Context::Search,
        Action::SearchInsert,
        Some(Help::new(
            Category::Search,
            "search-type",
            "Type in the search query",
        )),
    ),
    // -- Create-playlist text entry ----------------------------------------
    Binding::new(
        Context::CreatePlaylist,
        KeyCode::Enter,
        Ctrl::Any,
        Action::CreateSubmit,
        help(
            Category::CreatePlaylist,
            "create-submit",
            "Submit the typed playlist name",
        ),
    ),
    Binding::new(
        Context::CreatePlaylist,
        KeyCode::Esc,
        Ctrl::Any,
        Action::CreateCancel,
        help(
            Category::CreatePlaylist,
            "create-cancel",
            "Cancel and clear the name",
        ),
    ),
    Binding::new(
        Context::CreatePlaylist,
        KeyCode::Backspace,
        Ctrl::Any,
        Action::CreateBackspace,
        help(
            Category::CreatePlaylist,
            "create-delete",
            "Delete the last character",
        ),
    ),
    Binding::any_char(
        Context::CreatePlaylist,
        Action::CreateInsert,
        Some(Help::new(
            Category::CreatePlaylist,
            "create-type",
            "Type the playlist name",
        )),
    ),
    // -- Add-to-playlist picker --------------------------------------------
    Binding::new(
        Context::AddPicker,
        KeyCode::Up,
        Ctrl::Any,
        Action::AddPickerPrevious,
        help(
            Category::AddPicker,
            "add-move",
            "Move through library playlists",
        ),
    ),
    Binding::new(
        Context::AddPicker,
        KeyCode::Down,
        Ctrl::Any,
        Action::AddPickerNext,
        help(
            Category::AddPicker,
            "add-move",
            "Move through library playlists",
        ),
    ),
    Binding::new(
        Context::AddPicker,
        KeyCode::Char('j'),
        Ctrl::Any,
        Action::AddPickerNext,
        help(
            Category::AddPicker,
            "add-move",
            "Move through library playlists",
        ),
    ),
    Binding::new(
        Context::AddPicker,
        KeyCode::Char('k'),
        Ctrl::Any,
        Action::AddPickerPrevious,
        help(
            Category::AddPicker,
            "add-move",
            "Move through library playlists",
        ),
    ),
    Binding::new(
        Context::AddPicker,
        KeyCode::Enter,
        Ctrl::Any,
        Action::AddPickerConfirm,
        help(
            Category::AddPicker,
            "add-confirm",
            "Add the song to the selected playlist",
        ),
    ),
    Binding::new(
        Context::AddPicker,
        KeyCode::Esc,
        Ctrl::Any,
        Action::AddPickerCancel,
        help(Category::AddPicker, "add-cancel", "Close the picker"),
    ),
    // -- Browse-shelf picker ------------------------------------------------
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Up,
        Ctrl::Any,
        Action::BrowsePickerPrevious,
        help(
            Category::BrowsePicker,
            "browse-move",
            "Move through Browse shelves",
        ),
    ),
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Down,
        Ctrl::Any,
        Action::BrowsePickerNext,
        help(
            Category::BrowsePicker,
            "browse-move",
            "Move through Browse shelves",
        ),
    ),
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Char('j'),
        Ctrl::Any,
        Action::BrowsePickerNext,
        help(
            Category::BrowsePicker,
            "browse-move",
            "Move through Browse shelves",
        ),
    ),
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Char('k'),
        Ctrl::Any,
        Action::BrowsePickerPrevious,
        help(
            Category::BrowsePicker,
            "browse-move",
            "Move through Browse shelves",
        ),
    ),
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Enter,
        Ctrl::Any,
        Action::BrowsePickerConfirm,
        help(
            Category::BrowsePicker,
            "browse-confirm",
            "Open the selected shelf",
        ),
    ),
    Binding::new(
        Context::BrowsePicker,
        KeyCode::Esc,
        Ctrl::Any,
        Action::BrowsePickerCancel,
        help(Category::BrowsePicker, "browse-cancel", "Close the picker"),
    ),
    // -- Cheatsheet ---------------------------------------------------------
    Binding::new(
        Context::Help,
        KeyCode::Esc,
        Ctrl::Any,
        Action::CloseHelp,
        help(Category::Help, "close", "Close the cheatsheet"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Char('q'),
        Ctrl::Any,
        Action::CloseHelp,
        help(Category::Help, "close", "Close the cheatsheet"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Up,
        Ctrl::Any,
        Action::HelpLineUp,
        help(Category::Help, "line", "Scroll one line"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Down,
        Ctrl::Any,
        Action::HelpLineDown,
        help(Category::Help, "line", "Scroll one line"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Char('j'),
        Ctrl::Any,
        Action::HelpLineDown,
        help(Category::Help, "line", "Scroll one line"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Char('k'),
        Ctrl::Any,
        Action::HelpLineUp,
        help(Category::Help, "line", "Scroll one line"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::PageUp,
        Ctrl::Any,
        Action::HelpPageUp,
        help(Category::Help, "page", "Scroll one page"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::PageDown,
        Ctrl::Any,
        Action::HelpPageDown,
        help(Category::Help, "page", "Scroll one page"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::Home,
        Ctrl::Any,
        Action::HelpTop,
        help(Category::Help, "jump", "Jump to top / bottom"),
    ),
    Binding::new(
        Context::Help,
        KeyCode::End,
        Ctrl::Any,
        Action::HelpBottom,
        help(Category::Help, "jump", "Jump to top / bottom"),
    ),
];

/// Resolve a key in a context. Bindings are checked in order, so a text field's
/// wildcard must be last in its context (it is).
pub fn action_for(context: Context, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|binding| binding.context == context && binding.matches(code, modifiers))
        .map(|binding| binding.action)
}

/// One rendered row of the cheatsheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLine {
    pub keys: String,
    pub description: &'static str,
}

/// A titled group of cheatsheet rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: &'static str,
    pub lines: Vec<HelpLine>,
}

struct PendingLine {
    keys: Vec<String>,
    description: &'static str,
}

/// Build the cheatsheet from [`BINDINGS`], merging adjacent bindings that share
/// a help group (for example `↑` and `↓`).
pub fn help_sections() -> Vec<HelpSection> {
    let mut pending: Vec<Vec<PendingLine>> = (0..Category::ALL.len()).map(|_| Vec::new()).collect();
    let mut last_group: Vec<Option<(Context, &'static str)>> = vec![None; Category::ALL.len()];

    for binding in BINDINGS {
        let Some(help) = binding.help else {
            continue;
        };
        let index = help.category.index();
        let group = (binding.context, help.group);
        if last_group[index] == Some(group)
            && let Some(line) = pending[index].last_mut()
        {
            line.keys.push(binding.key_label());
            continue;
        }
        pending[index].push(PendingLine {
            keys: vec![binding.key_label()],
            description: help.description,
        });
        last_group[index] = Some(group);
    }

    Category::ALL
        .iter()
        .zip(pending)
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(category, lines)| HelpSection {
            title: category.title(),
            lines: lines
                .into_iter()
                .map(|line| HelpLine {
                    keys: format_keys(&line.keys),
                    description: line.description,
                })
                .collect(),
        })
        .collect()
}

/// Join a row's key labels. Consecutive single digits become `1–5` instead of
/// `1 / 2 / 3 / 4 / 5`.
fn format_keys(keys: &[String]) -> String {
    let consecutive_digits = keys.len() >= 3
        && keys
            .iter()
            .all(|key| key.len() == 1 && key.as_bytes()[0].is_ascii_digit())
        && keys
            .windows(2)
            .all(|pair| pair[0].as_bytes()[0] + 1 == pair[1].as_bytes()[0]);
    if consecutive_digits {
        format!("{}–{}", keys[0], keys[keys.len() - 1])
    } else {
        keys.join(" / ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(context: Context, code: KeyCode) -> Option<Action> {
        action_for(context, code, KeyModifiers::NONE)
    }

    fn ctrl(context: Context, code: KeyCode) -> Option<Action> {
        action_for(context, code, KeyModifiers::CONTROL)
    }

    #[test]
    fn normal_keys_dispatch_to_their_actions() {
        assert_eq!(key(Context::Normal, KeyCode::Char('q')), Some(Action::Quit));
        assert_eq!(
            key(Context::Normal, KeyCode::Char('?')),
            Some(Action::OpenHelp)
        );
        assert_eq!(
            key(Context::Normal, KeyCode::Char('/')),
            Some(Action::FocusSearch)
        );
        assert_eq!(key(Context::Normal, KeyCode::Up), Some(Action::PreviousRow));
        assert_eq!(key(Context::Normal, KeyCode::Down), Some(Action::NextRow));
        assert_eq!(
            key(Context::Normal, KeyCode::Char(' ')),
            Some(Action::TogglePlay)
        );
        assert_eq!(
            key(Context::Normal, KeyCode::Char('0')),
            None,
            "only 1–5 open browse sections"
        );
        for (digit, section) in [('1', 0), ('2', 1), ('3', 2), ('4', 3), ('5', 4)] {
            assert_eq!(
                key(Context::Normal, KeyCode::Char(digit)),
                Some(Action::OpenBrowseSection(section))
            );
        }
    }

    #[test]
    fn ctrl_twins_do_not_shadow_plain_keys() {
        assert_eq!(
            key(Context::Normal, KeyCode::Char('r')),
            Some(Action::RemoveFromPlaylist)
        );
        assert_eq!(
            ctrl(Context::Normal, KeyCode::Char('r')),
            Some(Action::CycleRepeat)
        );
        assert_eq!(
            key(Context::Normal, KeyCode::Char('a')),
            Some(Action::OpenAddPicker)
        );
        assert_eq!(
            ctrl(Context::Normal, KeyCode::Char('a')),
            Some(Action::ToggleAutoQueue)
        );
        assert_eq!(key(Context::Normal, KeyCode::Char('s')), None);
        assert_eq!(
            ctrl(Context::Normal, KeyCode::Char('s')),
            Some(Action::ToggleShuffle)
        );
        assert_eq!(
            ctrl(Context::Global, KeyCode::Char('c')),
            Some(Action::Quit)
        );
        assert_eq!(key(Context::Global, KeyCode::Char('c')), None);
    }

    #[test]
    fn text_modes_capture_their_own_keys() {
        assert_eq!(
            key(Context::Search, KeyCode::Char('?')),
            Some(Action::SearchInsert),
            "'?' is text while the search field owns the keys"
        );
        assert_eq!(
            key(Context::Search, KeyCode::Enter),
            Some(Action::SearchSubmit)
        );
        assert_eq!(
            key(Context::Search, KeyCode::Esc),
            Some(Action::SearchCancel)
        );
        assert_eq!(
            key(Context::Search, KeyCode::Backspace),
            Some(Action::SearchBackspace)
        );
        assert_eq!(
            key(Context::Search, KeyCode::Tab),
            None,
            "text fields do not cycle filters"
        );
        assert_eq!(
            key(Context::CreatePlaylist, KeyCode::Char('x')),
            Some(Action::CreateInsert)
        );
        assert_eq!(
            key(Context::CreatePlaylist, KeyCode::Enter),
            Some(Action::CreateSubmit)
        );
    }

    #[test]
    fn pickers_own_j_k_and_arrows() {
        assert_eq!(
            key(Context::AddPicker, KeyCode::Char('j')),
            Some(Action::AddPickerNext)
        );
        assert_eq!(
            key(Context::AddPicker, KeyCode::Char('k')),
            Some(Action::AddPickerPrevious)
        );
        assert_eq!(
            key(Context::AddPicker, KeyCode::Enter),
            Some(Action::AddPickerConfirm)
        );
        assert_eq!(
            key(Context::AddPicker, KeyCode::Esc),
            Some(Action::AddPickerCancel)
        );
        assert_eq!(
            key(Context::BrowsePicker, KeyCode::Down),
            Some(Action::BrowsePickerNext)
        );
        assert_eq!(
            key(Context::BrowsePicker, KeyCode::Up),
            Some(Action::BrowsePickerPrevious)
        );
        assert_eq!(
            key(Context::BrowsePicker, KeyCode::Enter),
            Some(Action::BrowsePickerConfirm)
        );
        assert_eq!(
            key(Context::BrowsePicker, KeyCode::Esc),
            Some(Action::BrowsePickerCancel)
        );
        assert_eq!(
            key(Context::AddPicker, KeyCode::Char('q')),
            None,
            "q does not quit from a picker"
        );
    }

    #[test]
    fn help_overlay_owns_its_keys() {
        assert_eq!(
            key(Context::Help, KeyCode::Char('q')),
            Some(Action::CloseHelp)
        );
        assert_eq!(key(Context::Help, KeyCode::Esc), Some(Action::CloseHelp));
        assert_eq!(
            key(Context::Help, KeyCode::Char('j')),
            Some(Action::HelpLineDown)
        );
        assert_eq!(
            key(Context::Help, KeyCode::Char('k')),
            Some(Action::HelpLineUp)
        );
        assert_eq!(
            key(Context::Help, KeyCode::PageDown),
            Some(Action::HelpPageDown)
        );
        assert_eq!(
            key(Context::Help, KeyCode::PageUp),
            Some(Action::HelpPageUp)
        );
        assert_eq!(key(Context::Help, KeyCode::Home), Some(Action::HelpTop));
        assert_eq!(key(Context::Help, KeyCode::End), Some(Action::HelpBottom));
        assert_eq!(
            key(Context::Help, KeyCode::Char('?')),
            None,
            "the open key is not a second toggle"
        );
    }

    #[test]
    fn every_help_row_is_populated_and_contexts_are_covered() {
        let sections = help_sections();
        assert_eq!(sections.len(), Category::ALL.len());
        for section in &sections {
            assert!(!section.lines.is_empty(), "{} is empty", section.title);
            for line in &section.lines {
                assert!(
                    !line.keys.is_empty(),
                    "{} has an empty key row",
                    section.title
                );
                assert!(
                    !line.description.is_empty(),
                    "{} has an empty description",
                    section.title
                );
            }
        }
        let titles: Vec<_> = sections.iter().map(|section| section.title).collect();
        assert!(titles.contains(&"Search input"));
        assert!(titles.contains(&"Create playlist"));
        assert!(titles.contains(&"Add to playlist picker"));
        assert!(titles.contains(&"Browse shelf picker"));
        assert!(titles.contains(&"Cheatsheet"));
    }

    #[test]
    fn grouped_keys_are_merged_and_digits_are_condensed() {
        let sections = help_sections();
        let navigation = sections
            .iter()
            .find(|section| section.title == "Navigation")
            .unwrap();
        assert!(navigation.lines.iter().any(|line| line.keys == "↑ / ↓"));
        assert!(navigation.lines.iter().any(|line| line.keys == "1–5"));
        assert!(
            navigation
                .lines
                .iter()
                .any(|line| line.keys == "Tab / Shift+Tab")
        );

        let add = sections
            .iter()
            .find(|section| section.title == "Add to playlist picker")
            .unwrap();
        assert!(add.lines.iter().any(|line| line.keys == "↑ / ↓ / j / k"));

        let playback = sections
            .iter()
            .find(|section| section.title == "Playback")
            .unwrap();
        for keys in ["Ctrl+A", "Ctrl+R", "Ctrl+S"] {
            assert!(
                playback.lines.iter().any(|line| line.keys == keys),
                "missing {keys}"
            );
        }
    }

    #[test]
    fn every_binding_has_a_help_row() {
        // This is the anti-drift check: a new binding without help metadata
        // would silently disappear from the cheatsheet.
        for binding in BINDINGS {
            assert!(
                binding.help.is_some(),
                "{:?} binding {:?} has no help",
                binding.context,
                binding.key_label()
            );
        }
    }
}
