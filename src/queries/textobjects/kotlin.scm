(class_declaration
  [
    (class_body)
    (enum_class_body)
  ]? @class.inner) @class.outer

[
  (function_declaration
    (function_body) @function.inner)
  (getter
    (function_body) @function.inner)
  (setter
    (function_body) @function.inner)
  (primary_constructor)
] @function.outer

(primary_constructor) @function.inner

[
  (parameter
    (simple_identifier) @parameter.inner)
  (class_parameter
    (simple_identifier) @parameter.inner)
] @parameter.outer

(value_arguments
  "," @parameter.outer
  .
  (value_argument) @parameter.inner @parameter.outer)

(value_arguments
  .
  (value_argument) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(value_arguments
  (value_argument) @parameter.outer
  .
  "," @parameter.outer .)

[
  (line_comment)
  (multiline_comment)
] @comment.outer
