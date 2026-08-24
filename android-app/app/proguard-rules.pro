# Keep all JNI entry points reachable — R8 must not strip them or the
# native lib will fail to bind by name at runtime.
-keep class com.pydmg.neogeo.NativeBridge { *; }
-keepclasseswithmembernames class * {
    native <methods>;
}
# Custom views inflated from XML layouts by name.
-keep class com.pydmg.neogeo.EmulatorView { <init>(...); }
-keep class com.pydmg.neogeo.JoystickView { <init>(...); }
