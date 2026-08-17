; Red-owned indentation query v1.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
[(comment) (expandable_here_string_literal) (expandable_string_literal) (string_literal) (verbatim_here_string_characters) (verbatim_string_characters)] @indent.ignore
