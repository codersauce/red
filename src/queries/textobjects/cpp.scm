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

(class_specifier
  body: (_) @class.inner) @class.outer

(field_declaration
  type: (enum_specifier)
  default_value: (initializer_list) @class.inner) @class.outer

(template_declaration
  (function_definition)) @function.outer

(template_declaration
  (struct_specifier)) @class.outer

(template_declaration
  (class_specifier)) @class.outer

(lambda_capture_specifier
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(lambda_capture_specifier
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(template_argument_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(template_argument_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(template_parameter_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(template_parameter_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(parameter_list
  "," @parameter.outer
  .
  (optional_parameter_declaration) @parameter.inner @parameter.outer)

(parameter_list
  .
  (optional_parameter_declaration) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(initializer_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(initializer_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(new_expression
  (argument_list) @call.inner) @call.outer
