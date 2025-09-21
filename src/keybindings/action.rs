// Search-related actions are fully configurable through keybindings:
// - Search: Toggle search mode (default: Ctrl+F)
// - CancelSearch: Exit search mode (handled by Escape)
// - NextMatch: Navigate to next search result (default: n, Enter, Tab)
// - PrevMatch: Navigate to previous search result (default: Shift+n, Shift+Tab)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    History(HistDirection),
    ToEdge(VertDirection),
    Scroll(VertDirection),
    Page(VertDirection),
    Zoom(Zoom),
    Copy,
    Help,
    Search,
    CancelSearch,
    NextMatch,
    PrevMatch,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistDirection {
    Next,
    Prev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VertDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    Reset,
}
