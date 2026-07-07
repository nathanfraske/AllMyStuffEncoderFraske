// Hide the iOS keyboard's input-accessory bar (the prev / next / Done toolbar
// that iOS auto-adds above every WKWebView text field). The console's soft
// keyboard drives a hidden input, and that bar just gets in the way.
//
// There is no web or Tauri-config way to remove it: the bar belongs to the
// private `WKContentView` that hosts WKWebView's editable content. The standard
// fix — used by Cordova/Ionic/Capacitor's keyboard plugins for years — is to
// swizzle `-inputAccessoryView` on that class to return nil.
//
// This is a self-installing Objective-C file: `+load` runs automatically when
// the binary loads, before `main`, so nothing has to reference or call it. It
// just has to be compiled into the app target. `patch-xcode-project.sh` copies
// it into the generated `gen/apple/Sources/<app>/` folder after `tauri ios
// init`; with Xcode 16 synchronized groups that's enough, otherwise add it to
// the target once by hand. Removing the file restores the default bar.

#import <UIKit/UIKit.h>
#import <objc/runtime.h>

@interface AMSHideKeyboardAccessory : NSObject
@end

@implementation AMSHideKeyboardAccessory

+ (void)load {
  Class cls = NSClassFromString(@"WKContentView");
  if (cls == Nil) {
    return;
  }
  SEL sel = NSSelectorFromString(@"inputAccessoryView");
  IMP imp = imp_implementationWithBlock(^UIView *(id _self) {
    return nil;
  });
  Method method = class_getInstanceMethod(cls, sel);
  if (method != NULL) {
    method_setImplementation(method, imp);
  } else {
    // `@@:` = returns object, takes (self, _cmd) — a plain getter signature.
    class_addMethod(cls, sel, imp, "@@:");
  }
}

@end
