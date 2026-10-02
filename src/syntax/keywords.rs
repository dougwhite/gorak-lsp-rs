//! Reserved words shared by parsing, completion and rename validation.
pub const WORDS: &str = "all and array as asc begin by byref call callframe callproc case close commit connect continue create declare delete desc disconnect distinct do drop else elseif end endcase enddeclare endfor endif endloop endwhile execute exists exit fetch for from getevent gotoframe group having if immediate in include initialize inquire_sql insert into is like loop message method not null of on open openframe or order permit private procedure public qualification raiseevent repeat resume return returning rollback savepoint select set sleep then to union update values where while with";
pub fn contains(name: &str) -> bool {
    static WORDS_SET: std::sync::OnceLock<std::collections::HashSet<&'static str>> =
        std::sync::OnceLock::new();
    WORDS_SET
        .get_or_init(|| WORDS.split_whitespace().collect())
        .contains(name)
}
