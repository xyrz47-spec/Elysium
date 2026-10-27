buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.3.1")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.10")
    }
}

allprojects {
    repositories {
        google()
        mavenCentral()
    }
    // OneDrive 동기화 폴더에 빌드 산출물이 쌓이지 않도록 밖으로 둔다.
    layout.buildDirectory.set(file("C:/Users/HOYA/.elysium-build/android/${project.name}"))
}

tasks.register("clean").configure {
    delete("build")
}

