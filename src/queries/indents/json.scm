; Red-owned indentation query v1.
["{" "["] @indent.begin
["}" "]"] @indent.end
[(comment) (string)] @indent.ignore
