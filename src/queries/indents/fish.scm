; Red-owned indentation query v1.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
([(if_statement "if" @indent.begin) (for_statement "for" @indent.begin) (while_statement "while" @indent.begin) (function_definition "function" @indent.begin) (begin_statement "begin" @indent.begin) (switch_statement "switch" @indent.begin)] (#set! indent.match "end"))
("end" @indent.end (#set! indent.match "end"))
(["else" "case"] @indent.branch (#set! indent.match "end"))
[(comment) (double_quote_string) (single_quote_string)] @indent.ignore
