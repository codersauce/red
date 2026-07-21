(component
  (core module $main
    (type (func (param i32) (result i32)))
    (type (func (result i32)))
    (type (func (param i32)))
    (import "[export]fixture:resource/api" "[resource-new]item"
      (func $new (type 0)))
    (import "[export]fixture:resource/api" "[resource-drop]item"
      (func $drop (type 2)))
    (export "fixture#new" (func $make))
    (export "fixture#value" (func $value))
    (export "fixture#consume" (func $consume))
    (func $make (type 1) (result i32)
      i32.const 100
      call $new)
    (func $value (type 0) (param i32) (result i32)
      local.get 0)
    (func $consume (type 2) (param i32)
      local.get 0
      call $drop))

  (type $item (resource (rep i32)))
  (type $owned-item (own $item))
  (type $borrowed-item (borrow $item))
  (core func $resource-new (canon resource.new $item))
  (core func $resource-drop (canon resource.drop $item))
  (core instance $resource-intrinsics
    (export "[resource-new]item" (func $resource-new))
    (export "[resource-drop]item" (func $resource-drop)))
  (core instance $main-instance
    (instantiate $main
      (with "[export]fixture:resource/api" (instance $resource-intrinsics))))

  (component $api-component
    (import "item-type" (type (sub resource)))
    (export "item" (type 0))
    (type $handle (own 1))
    (export "handle" (type $handle)))
  (instance $api
    (instantiate $api-component
      (with "item-type" (type $item))))
  (export "fixture:resource/api" (instance $api))

  (alias core export $main-instance "fixture#new" (core func $new-item))
  (func $new-item (result $owned-item)
    (canon lift (core func $new-item)))
  (alias core export $main-instance "fixture#value" (core func $item-value))
  (func $item-value
    (param "item" $borrowed-item)
    (result s32)
    (canon lift (core func $item-value)))
  (alias core export $main-instance "fixture#consume" (core func $consume-item))
  (func $consume-item
    (param "item" $owned-item)
    (canon lift (core func $consume-item)))
  (alias export $api "item" (type $exported-item))
  (alias export $api "handle" (type $exported-handle))

  (component $factory-component
    (import "item-type" (type (sub resource)))
    (type $owned (own 0))
    (type $borrowed (borrow 0))
    (import "handle-type" (type $handle (eq $owned)))
    (import "new-item" (func (result $handle)))
    (import "item-value" (func (param "item" $borrowed) (result s32)))
    (import "consume-item" (func (param "item" $owned)))
    (export "handle" (type $handle))
    (export "new-item" (func 0))
    (export "item-value" (func 1))
    (export "consume-item" (func 2)))
  (instance $factory
    (instantiate $factory-component
      (with "item-type" (type $exported-item))
      (with "handle-type" (type $exported-handle))
      (with "new-item" (func $new-item))
      (with "item-value" (func $item-value))
      (with "consume-item" (func $consume-item))))
  (export "fixture:resource/factory" (instance $factory)))
