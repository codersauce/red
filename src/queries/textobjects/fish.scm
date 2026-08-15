; assignment
(command
  name: (word) @_command
  argument: (word) @_varname @assignment.lhs @assignment.inner
  argument: (_)* @assignment.rhs
  (#not-match? @_varname "^[-].*")
  (#eq? @_command "set")) @assignment.outer

(command
  name: (word) @_name
  argument: (_)* @assignment.inner
  (#eq? @_name "set"))

; block
([
  (case_clause)
  (if_statement)
  (switch_statement)
  (else_clause)
  (for_statement)
  (while_statement)
]) @block.outer

; call
; call.inner doesn't work because it can't select *all* arguments
(command) @call.outer

; Comment interiors are normalized from the outer range by Red's syntax service.
(comment) @comment.outer

; conditional
(if_statement
  (command) @conditional.inner) @conditional.outer

(switch_statement
  (_) @conditional.inner) @conditional.outer

; function
(function_definition) @function.outer

; loop
(for_statement
  (_) @loop.inner) @loop.outer

(while_statement
  condition: (command)
  (command) @loop.inner) @loop.outer

; number
[
  (integer)
  (float)
] @number.inner

; parameter
(command
  argument: (_) @parameter.outer)

; return
(return
  (_) @return.inner) @return.outer

; statement
(command) @statement.outer
