; Red-owned indentation query v1.
["{" "(" "["] @indent.begin
["}" ")" "]"] @indent.end
((if_statement "then" @indent.begin) (#set! indent.match "fi"))
("fi" @indent.end (#set! indent.match "fi"))
(["elif" "else"] @indent.branch (#set! indent.match "fi"))
("do" @indent.begin (#set! indent.match "done"))
("done" @indent.end (#set! indent.match "done"))
("case" @indent.begin (#set! indent.match "esac"))
("esac" @indent.end (#set! indent.match "esac"))
[(ansi_c_string) (comment) (heredoc_body) (raw_string) (regex) (string) (translated_string)] @indent.ignore
