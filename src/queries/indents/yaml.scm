; Red-owned indentation query v1.
["{" "["] @indent.begin
["}" "]"] @indent.end
[(block_scalar) (comment) (string_scalar)] @indent.ignore
