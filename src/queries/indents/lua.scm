; Red-owned indentation query v1.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
([(function_declaration "function" @indent.begin) (function_definition "function" @indent.begin) (if_statement "then" @indent.begin) (do_statement "do" @indent.begin) (for_statement "do" @indent.begin) (while_statement "do" @indent.begin)] (#set! indent.match "end"))
("end" @indent.end (#set! indent.match "end"))
(["else" "elseif"] @indent.branch (#set! indent.match "end"))
("repeat" @indent.begin (#set! indent.match "until"))
("until" @indent.end (#set! indent.match "until"))
[(comment) (string)] @indent.ignore
