(class_declaration
  body: (class_body) @class.inner) @class.outer

(method_declaration) @function.outer

(method_declaration
  body: (block
    .
    "{"
    _+ @function.inner
    "}"))

(constructor_declaration) @function.outer

(constructor_declaration
  body: (constructor_body
    .
    "{"
    _+ @function.inner
    "}"))

(method_invocation) @call.outer

(method_invocation
  arguments: (argument_list
    .
    "("
    _+ @call.inner
    ")"))

(formal_parameters
  "," @parameter.outer
  .
  (formal_parameter) @parameter.inner @parameter.outer)

(formal_parameters
  .
  (formal_parameter) @parameter.inner @parameter.outer
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

[
  (line_comment)
  (block_comment)
] @comment.outer
