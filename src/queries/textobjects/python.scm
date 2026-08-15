(decorated_definition
  (function_definition)) @function.outer

(function_definition
  body: (block)? @function.inner) @function.outer

(decorated_definition
  (class_definition)) @class.outer

(class_definition
  body: (block)? @class.inner) @class.outer

(call) @call.outer

(call
  arguments: (argument_list
    .
    "("
    _+ @call.inner
    ")"))

(parameters
  "," @parameter.outer
  .
  [
    (identifier)
    (tuple)
    (typed_parameter)
    (default_parameter)
    (typed_default_parameter)
    (dictionary_splat_pattern)
    (list_splat_pattern)
  ] @parameter.inner @parameter.outer)

(parameters
  .
  [
    (identifier)
    (tuple)
    (typed_parameter)
    (default_parameter)
    (typed_default_parameter)
    (dictionary_splat_pattern)
    (list_splat_pattern)
  ] @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(lambda_parameters
  "," @parameter.outer
  .
  [
    (identifier)
    (tuple)
    (typed_parameter)
    (default_parameter)
    (typed_default_parameter)
    (dictionary_splat_pattern)
    (list_splat_pattern)
  ] @parameter.inner @parameter.outer)

(lambda_parameters
  .
  [
    (identifier)
    (tuple)
    (typed_parameter)
    (default_parameter)
    (typed_default_parameter)
    (dictionary_splat_pattern)
    (list_splat_pattern)
  ] @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(tuple
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(tuple
  "("
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(set
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(set
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(dictionary
  .
  (pair) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(dictionary
  "," @parameter.outer
  .
  (pair) @parameter.inner @parameter.outer)

(argument_list
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(argument_list
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(subscript
  "["
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(subscript
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(import_statement
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(import_statement
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(import_from_statement
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(import_from_statement
  "import"
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)

(comment) @comment.outer
