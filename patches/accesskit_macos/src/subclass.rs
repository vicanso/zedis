// Copyright 2022 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

use accesskit::{ActionHandler, ActivationHandler, TreeUpdate};
use objc2::{
    ClassType, DeclaredClass,
    declare::ClassBuilder,
    declare_class,
    ffi::{
        OBJC_ASSOCIATION_RETAIN_NONATOMIC, objc_getAssociatedObject, objc_setAssociatedObject,
        object_setClass,
    },
    msg_send, msg_send_id,
    mutability::InteriorMutable,
    rc::Id,
    runtime::{AnyClass, AnyObject, Sel},
    sel,
};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSArray, NSObject, NSPoint};
use std::{cell::RefCell, ffi::c_void, sync::Mutex};

use crate::{Adapter, event::QueuedEvents};

static SUBCLASSES: Mutex<Vec<(&'static AnyClass, &'static AnyClass)>> = Mutex::new(Vec::new());

static ASSOCIATED_OBJECT_KEY: u8 = 0;

fn associated_object_key() -> *const c_void {
    (&ASSOCIATED_OBJECT_KEY as *const u8).cast()
}

struct AssociatedObjectState {
    adapter: Adapter,
    activation_handler: Box<dyn ActivationHandler>,
}

struct AssociatedObjectIvars {
    state: RefCell<AssociatedObjectState>,
    prev_class: &'static AnyClass,
}

declare_class!(
    struct AssociatedObject;

    unsafe impl ClassType for AssociatedObject {
        type Super = NSObject;
        type Mutability = InteriorMutable;
        const NAME: &'static str = "AccessKitSubclassAssociatedObject";
    }

    impl DeclaredClass for AssociatedObject {
        type Ivars = AssociatedObjectIvars;
    }
);

impl AssociatedObject {
    fn new(
        adapter: Adapter,
        activation_handler: impl 'static + ActivationHandler,
        prev_class: &'static AnyClass,
    ) -> Id<Self> {
        let state = RefCell::new(AssociatedObjectState {
            adapter,
            activation_handler: Box::new(activation_handler),
        });
        let this = Self::alloc().set_ivars(AssociatedObjectIvars { state, prev_class });

        unsafe { msg_send_id![super(this), init] }
    }
}

fn associated_object(view: &NSView) -> Option<&AssociatedObject> {
    unsafe {
        (objc_getAssociatedObject(view as *const NSView as *const _, associated_object_key())
            as *const AssociatedObject)
            .as_ref()
    }
}

// Unlike `[view class]`, this sees the subclass that key-value observing
// hides by overriding `class`.
fn runtime_class(view: &NSView) -> &'static AnyClass {
    let object: &AnyObject = view;
    object.class()
}

// Key-value observing may have subclassed our subclass in turn.
fn installed_subclass(view: &NSView) -> Option<(&'static AnyClass, &'static AnyClass)> {
    let subclasses = SUBCLASSES.lock().unwrap();
    let mut class: Option<&AnyClass> = Some(runtime_class(view));
    while let Some(current) = class {
        if let Some(entry) = subclasses
            .iter()
            .find(|entry| std::ptr::eq(entry.1, current))
        {
            return Some(*entry);
        }
        class = current.superclass();
    }
    None
}

fn prev_class(view: &NSView) -> &'static AnyClass {
    installed_subclass(view).unwrap().0
}

// Some view classes, like the one in winit 0.27, assume that they are the
// lowest subclass, and call [self superclass] to get their superclass.
// Give them the answer they need.
unsafe extern "C" fn superclass(this: &NSView, _cmd: Sel) -> Option<&AnyClass> {
    let prev_class = match associated_object(this) {
        Some(associated) => associated.ivars().prev_class,
        None => prev_class(this),
    };
    prev_class.superclass()
}

unsafe extern "C" fn children(this: &NSView, _cmd: Sel) -> *mut NSArray<NSObject> {
    let Some(associated) = associated_object(this) else {
        return unsafe { msg_send![super(this, prev_class(this)), accessibilityChildren] };
    };
    let mut state = associated.ivars().state.borrow_mut();
    let state_mut = &mut *state;
    state_mut
        .adapter
        .view_children(&mut *state_mut.activation_handler)
}

unsafe extern "C" fn focus(this: &NSView, _cmd: Sel) -> *mut NSObject {
    let Some(associated) = associated_object(this) else {
        return unsafe { msg_send![super(this, prev_class(this)), accessibilityFocusedUIElement] };
    };
    let mut state = associated.ivars().state.borrow_mut();
    let state_mut = &mut *state;
    state_mut.adapter.focus(&mut *state_mut.activation_handler)
}

unsafe extern "C" fn hit_test(this: &NSView, _cmd: Sel, point: NSPoint) -> *mut NSObject {
    let Some(associated) = associated_object(this) else {
        return unsafe { msg_send![super(this, prev_class(this)), accessibilityHitTest: point] };
    };
    let mut state = associated.ivars().state.borrow_mut();
    let state_mut = &mut *state;
    state_mut
        .adapter
        .hit_test(point, &mut *state_mut.activation_handler)
}

fn subclass_of(prev_class: &'static AnyClass) -> &'static AnyClass {
    let mut subclasses = SUBCLASSES.lock().unwrap();
    match subclasses
        .iter()
        .find(|entry| std::ptr::eq(entry.0, prev_class))
    {
        Some(entry) => entry.1,
        None => {
            let name = format!("AccessKitSubclassOf{}", prev_class.name());
            let mut builder = ClassBuilder::new(&name, prev_class).unwrap();
            unsafe {
                builder.add_method(
                    sel!(superclass),
                    superclass as unsafe extern "C" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(accessibilityChildren),
                    children as unsafe extern "C" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(accessibilityFocusedUIElement),
                    focus as unsafe extern "C" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(accessibilityHitTest:),
                    hit_test as unsafe extern "C" fn(_, _, _) -> _,
                );
            }
            let class = builder.register();
            subclasses.push((prev_class, class));
            class
        }
    }
}

/// Uses dynamic Objective-C subclassing to implement the `NSView`
/// accessibility methods when normal subclassing isn't an option.
pub struct SubclassingAdapter {
    view: Id<NSView>,
    associated: Id<AssociatedObject>,
}

impl SubclassingAdapter {
    /// Create an adapter that dynamically subclasses the specified view.
    /// This must be done before the view is shown or focused for
    /// the first time, and before anything observes the view with
    /// key-value observing.
    ///
    /// The action handler will always be called on the main thread.
    ///
    /// # Safety
    ///
    /// `view` must be a valid, unreleased pointer to an `NSView`.
    pub unsafe fn new(
        view: *mut c_void,
        activation_handler: impl 'static + ActivationHandler,
        action_handler: impl 'static + ActionHandler,
    ) -> Self {
        let view = view as *mut NSView;
        let retained_view = unsafe { Id::retain(view) }.unwrap();
        Self::new_internal(retained_view, activation_handler, action_handler)
    }

    fn new_internal(
        retained_view: Id<NSView>,
        activation_handler: impl 'static + ActivationHandler,
        action_handler: impl 'static + ActionHandler,
    ) -> Self {
        let view = Id::as_ptr(&retained_view) as *mut NSView;
        if !unsafe {
            objc_getAssociatedObject(view as *const NSView as *const _, associated_object_key())
        }
        .is_null()
        {
            panic!("subclassing adapter already instantiated on view {view:?}");
        }
        // A previous adapter may have left its subclass on the view,
        // if key-value observing had subclassed it in turn.
        let installed = installed_subclass(unsafe { &*view });
        if installed.is_none() {
            // Key-value observing crashes when it adds or removes an observer
            // while our subclass sits on top of its own.
            let reported_class: &AnyClass = unsafe { msg_send![view, class] };
            let actual_class = runtime_class(unsafe { &*view });
            if !std::ptr::eq(reported_class, actual_class) {
                panic!(
                    "view {view:?} is already observed with key-value observing (its class {} reports itself as {}); create the subclassing adapter before anything observes the view",
                    actual_class.name(),
                    reported_class.name()
                );
            }
        }
        let adapter = unsafe { Adapter::new(view as *mut c_void, false, action_handler) };
        let (prev_class, subclass) = installed.unwrap_or_else(|| {
            let prev_class = runtime_class(unsafe { &*view });
            (prev_class, subclass_of(prev_class))
        });
        let associated = AssociatedObject::new(adapter, activation_handler, prev_class);
        unsafe {
            objc_setAssociatedObject(
                view as *mut _,
                associated_object_key(),
                Id::as_ptr(&associated) as *mut _,
                OBJC_ASSOCIATION_RETAIN_NONATOMIC,
            )
        };
        // SAFETY: Changing the view's class is only safe because
        // the subclass doesn't add any instance variables;
        // it uses an associated object instead.
        if installed.is_none() {
            unsafe { object_setClass(view as *mut _, (subclass as *const AnyClass).cast()) };
        }
        Self {
            view: retained_view,
            associated,
        }
    }

    /// Create an adapter that dynamically subclasses the content view
    /// of the specified window.
    ///
    /// The action handler will always be called on the main thread.
    ///
    /// # Safety
    ///
    /// `window` must be a valid, unreleased pointer to an `NSWindow`.
    ///
    /// # Panics
    ///
    /// This function panics if the specified window doesn't currently have
    /// a content view.
    pub unsafe fn for_window(
        window: *mut c_void,
        activation_handler: impl 'static + ActivationHandler,
        action_handler: impl 'static + ActionHandler,
    ) -> Self {
        let window = unsafe { &*(window as *const NSWindow) };
        let retained_view = window.contentView().unwrap();
        Self::new_internal(retained_view, activation_handler, action_handler)
    }

    /// If and only if the tree has been initialized, call the provided function
    /// and apply the resulting update. Note: If the caller's implementation of
    /// [`ActivationHandler::request_initial_tree`] initially returned `None`,
    /// the [`TreeUpdate`] returned by the provided function must contain
    /// a full tree.
    ///
    /// If a [`QueuedEvents`] instance is returned, the caller must call
    /// [`QueuedEvents::raise`] on it.
    pub fn update_if_active(
        &mut self,
        update_factory: impl FnOnce() -> TreeUpdate,
    ) -> Option<QueuedEvents> {
        let mut state = self.associated.ivars().state.borrow_mut();
        state.adapter.update_if_active(update_factory)
    }

    /// Update the tree state based on whether the window is focused.
    ///
    /// If a [`QueuedEvents`] instance is returned, the caller must call
    /// [`QueuedEvents::raise`] on it.
    pub fn update_view_focus_state(&mut self, is_focused: bool) -> Option<QueuedEvents> {
        let mut state = self.associated.ivars().state.borrow_mut();
        state.adapter.update_view_focus_state(is_focused)
    }
}

impl Drop for SubclassingAdapter {
    fn drop(&mut self) {
        let prev_class = self.associated.ivars().prev_class;
        let view = Id::as_ptr(&self.view) as *mut NSView;
        unsafe {
            objc_setAssociatedObject(
                view as *mut _,
                associated_object_key(),
                std::ptr::null_mut(),
                OBJC_ASSOCIATION_RETAIN_NONATOMIC,
            )
        };
        // Key-value observing may have subclassed our subclass in turn
        // (AppKit observes the first responder), and resetting the class
        // would then break those observers. Our subclass is harmless
        // without the associated object.
        let view = unsafe { &*view };
        let is_our_subclass = SUBCLASSES
            .lock()
            .unwrap()
            .iter()
            .any(|entry| std::ptr::eq(entry.1, runtime_class(view)));
        if is_our_subclass {
            unsafe {
                object_setClass(
                    view as *const _ as *mut _,
                    (prev_class as *const AnyClass).cast(),
                )
            };
        }
    }
}
