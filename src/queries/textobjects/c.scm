(declaration
  declarator: (function_declarator)) @function.outer

(function_definition
  body: (compound_statement)) @function.outer

(function_definition
  body: (compound_statement
    .
    "{"
    _+ @function.inner
    "}"))

(struct_specifier
  body: (_) @class.inner) @class.outer

(enum_specifier
  body: (_) @class.inner) @class.outer

(comment) @comment.outer

(call_expression) @call.outer

(call_expression
  arguments: (argument_list
    .
    "("
    _+ @call.inner
    ")"))

(parameter_list
  "," @parameter.outer
  .
  (parameter_declaration) @parameter.inner @parameter.outer)

(parameter_list
  .
  (parameter_declaration) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(argument_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(argument_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)
