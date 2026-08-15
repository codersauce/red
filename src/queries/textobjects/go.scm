(function_declaration
  body: (block
    .
    "{"
    _+ @function.inner
    "}"))

(func_literal
  body: (block
    .
    "{"
    _+ @function.inner
    "}"))

(method_declaration
  body: (block
    .
    "{"
    _+ @function.inner
    "}"))

(function_declaration) @function.outer

(func_literal
  (_)?) @function.outer

(method_declaration
  body: (block)?) @function.outer

(type_declaration
  (type_spec
    (type_identifier)
    (struct_type
      (field_declaration_list
        (_)?) @class.inner))) @class.outer

(type_declaration
  (type_spec
    (type_identifier)
    (interface_type) @class.inner)) @class.outer

(composite_literal
  (type_identifier)?
  (struct_type
    (_))?
  (literal_value
    (_)) @class.inner) @class.outer

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

(parameter_declaration
  name: (identifier)
  type: (_)) @parameter.inner

(parameter_list
  "," @parameter.outer
  .
  (variadic_parameter_declaration) @parameter.inner @parameter.outer)

(argument_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(argument_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)
