(function_definition
  body: (compound_statement
    .
    "{"
    _+ @function.inner
    "}"))

(function_definition) @function.outer

(anonymous_function
  body: (compound_statement
    .
    "{"
    _+ @function.inner
    "}"))

(anonymous_function) @function.outer

(method_declaration
  body: (compound_statement
    .
    "{"
    _+ @function.inner
    "}"))

(method_declaration) @function.outer

(trait_declaration
  body: (declaration_list
    .
    "{"
    _+ @class.inner
    "}"))

(trait_declaration) @class.outer

(interface_declaration
  body: (declaration_list
    .
    "{"
    _+ @class.inner
    "}"))

(interface_declaration) @class.outer

(enum_declaration
  body: (enum_declaration_list
    .
    "{"
    _+ @class.inner
    "}"))

(enum_declaration) @class.outer

(class_declaration
  body: (declaration_list
    .
    "{"
    _+ @class.inner
    "}"))

(class_declaration) @class.outer

(arguments
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(arguments
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(formal_parameters
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(formal_parameters
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(comment) @comment.outer

(function_call_expression) @call.outer

(member_call_expression) @call.outer

(nullsafe_member_call_expression) @call.outer

(scoped_call_expression) @call.outer

(function_call_expression
  arguments: (arguments
    .
    "("
    _+ @call.inner
    ")"))

(member_call_expression
  arguments: (arguments
    .
    "("
    _+ @call.inner
    ")"))

(nullsafe_member_call_expression
  arguments: (arguments
    .
    "("
    _+ @call.inner
    ")"))

(scoped_call_expression
  arguments: (arguments
    .
    "("
    _+ @call.inner
    ")"))
