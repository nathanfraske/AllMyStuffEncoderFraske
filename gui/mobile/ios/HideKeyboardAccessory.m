// Hide the two toolbars iOS stacks above the keyboard over a WKWebView text
// field — both of which just crowd the console's soft keyboard (it drives a
// hidden input, so neither bar has anything to offer):
//
//   1. inputAccessoryView — the prev / next / Done accessory bar (iPhone).
//   2. inputAssistantItem — the iPad "shortcuts bar" (the ‹ › … Done pill that
//      floats above the on-screen keyboard). This is a SEPARATE surface from
//      #1: returning nil from -inputAccessoryView does nothing to it, which is
//      why the pill kept showing on iPad after only #1 was suppressed. It's
//      driven by the responder's UITextInputAssistantItem, so we empty that
//      item's leading/trailing bar-button groups and the bar collapses.
//
// There is no web or Tauri-config way to remove either: both belong to the
// private `WKContentView` that hosts WKWebView's editable content. The standard
// fix — used by Cordova/Ionic/Capacitor's keyboard plugins for years — is to
// swizzle these getters on that class. This app is entirely a WKWebView (Tauri)
// with no native text fields, so touching WKContentView reaches every input
// there is and nothing else.
//
// This is a self-installing Objective-C file: `+load` runs automatically when
// the binary loads, before `main`, so nothing has to reference or call it. It
// just has to be compiled into the app target. `patch-xcode-project.sh` copies
// it into the generated `gen/apple/Sources/<app>/` folder after `tauri ios
// init`; with Xcode 16 synchronized groups that's enough, otherwise add it to
// the target once by hand. Removing the file restores the default bars.

#import <UIKit/UIKit.h>
#import <objc/runtime.h>

@interface AMSHideKeyboardAccessory : NSObject
@end

// Install `imp` as WKContentView's OWN implementation of `sel`, scoped to that
// class alone. class_addMethod adds an override only when WKContentView doesn't
// already define the selector itself (a method merely INHERITED from UIResponder
// doesn't count) — it returns NO exactly when the class has its own, and then
// method_setImplementation replaces that own method. Either branch touches only
// WKContentView, so UIResponder's shared method is never rewritten and the
// swizzle can't leak to unrelated responders (a native alert's text field, say).
// Returns the implementation `sel` resolved to just before the swap, for callers
// that need to chain through the original.
static IMP AMSInstallOverride(Class cls, SEL sel, IMP imp, const char *types) {
  IMP orig = class_getMethodImplementation(cls, sel);
  if (!class_addMethod(cls, sel, imp, types)) {
    method_setImplementation(class_getInstanceMethod(cls, sel), imp);
  }
  return orig;
}

@implementation AMSHideKeyboardAccessory

+ (void)load {
  Class cls = NSClassFromString(@"WKContentView");
  if (cls == Nil) {
    return;
  }

  // 1. The iPhone accessory bar → nil. (`@@:` = returns object, takes self +
  //    _cmd — a plain getter signature.)
  IMP accessory = imp_implementationWithBlock(^UIView *(id _self) {
    return nil;
  });
  AMSInstallOverride(cls, NSSelectorFromString(@"inputAccessoryView"), accessory, "@@:");

  // 2. The iPad shortcuts bar → keep the (non-nil) assistant item UIKit
  //    expects, but strip its button groups so nothing is left to draw. Chain
  //    through the original getter for the live item rather than fabricate one;
  //    capturing that IMP (and calling it directly, never via objc_msgSend) is
  //    what keeps the block from recursing into itself. `orig` is set right
  //    after install and read only later, when iOS queries the item.
  SEL asel = NSSelectorFromString(@"inputAssistantItem");
  __block UITextInputAssistantItem *(*orig)(id, SEL) = NULL;
  IMP assistant = imp_implementationWithBlock(^UITextInputAssistantItem *(id _self) {
    UITextInputAssistantItem *item = orig ? orig(_self, asel) : nil;
    if (item != nil) {
      item.leadingBarButtonGroups = @[];
      item.trailingBarButtonGroups = @[];
    }
    return item;
  });
  orig = (UITextInputAssistantItem *(*)(id, SEL))AMSInstallOverride(cls, asel, assistant, "@@:");
}

@end
