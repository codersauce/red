; Red indentation query v1. Strings and comments are opaque to delimiter rules.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
[(line_comment) (block_comment) (string_literal) (raw_string_literal) (char_literal)] @indent.ignore
