; Red-owned indentation query v1.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
[(comment) (regex) (string) (template_string)] @indent.ignore
