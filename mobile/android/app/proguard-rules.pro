# R8 keep-rules for the Capacitor release build (#556).
#
# History this file exists to prevent: with an empty rules file, `minifyEnabled
# true` let R8's optimizing pass merge `BridgeActivity` into `MainActivity` in
# the published APK, and the release build exited during startup on a real
# device. R8 was then turned off wholesale (`minifyEnabled false`), which fixed
# the crash but shipped an unoptimized bundle (Play "App optimization = Low").
# The correct fix is to keep the surface Capacitor discovers and invokes
# *reflectively* — R8 cannot see those uses, so it must be told — and leave R8
# on for everything else.
#
# For debugging, keep enough to read a release stack trace.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
# Annotations R8 must retain for the reflective lookups below to resolve.
-keepattributes *Annotation*, JavascriptInterface

# ── Capacitor bridge & plugin surface ────────────────────────────────────────
# Plugins are loaded by class name at runtime, so nothing statically references
# them — keep every Plugin subclass and the whole bridge package. Keeping the
# bridge package by name is also what stops R8 merging `BridgeActivity` into a
# subclass (the exact crash above): a kept class is neither renamed nor merged.
-keep public class * extends com.getcapacitor.Plugin
-keep class com.getcapacitor.** { *; }
-keep class com.capacitorjs.** { *; }
-keep class com.getcapacitor.BridgeActivity { *; }

# The `@CapacitorPlugin` / `@PluginMethod` annotated surface is resolved by the
# bridge reflectively; keep the annotated types and members whole.
-keep @com.getcapacitor.annotation.CapacitorPlugin public class * { *; }
-keepclassmembers class * {
    @com.getcapacitor.annotation.CapacitorPlugin *;
}
-keepclassmembers class * {
    @com.getcapacitor.PluginMethod public *;
}

# ── WebView JavaScript interfaces ────────────────────────────────────────────
# `@JavascriptInterface` methods are called from JS by name; R8 has no static
# caller for them and would strip or rename them. This covers both the bridge's
# own interfaces and MainActivity's `AndroidClipboard` / `AndroidVoice` bridges
# (VoiceBridge / ClipboardBridge, #523).
-keepclassmembers class * {
    @android.webkit.JavascriptInterface <methods>;
}

# ── Plugins in use (capacitor.settings.gradle) ───────────────────────────────
# @capacitor-community/speech-recognition — the audio/mic path with the prior
#   release-only crash history; keep it whole.
-keep class com.getcapacitor.community.speechrecognition.** { *; }
# @capacitor/app
-keep class com.capacitorjs.plugins.app.** { *; }
# @capacitor/push-notifications (+ the Firebase messaging surface it drives)
-keep class com.capacitorjs.plugins.pushnotifications.** { *; }
-keep class com.google.firebase.** { *; }
-keep class com.google.android.gms.** { *; }
# firebase-installations references a Kotlin `ktx` extension class that isn't on
# the classpath (the KTX artifact isn't pulled in). The reference is unused at
# runtime; suppress the R8 missing-class warning it raises. Generated into
# build/outputs/mapping/release/missing_rules.txt by AGP.
-dontwarn com.google.firebase.ktx.Firebase

# ── Cordova bridge ───────────────────────────────────────────────────────────
# Capacitor loads its cordova-plugin compatibility layer reflectively too.
-keep class org.apache.cordova.** { *; }
