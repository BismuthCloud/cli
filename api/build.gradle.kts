plugins {
    kotlin("jvm") version "1.9.21"
    kotlin("plugin.allopen") version "1.9.21"
    id("io.quarkus")
    application
}

repositories {
    mavenCentral()
    mavenLocal()
}

val quarkusPlatformGroupId: String by project
val quarkusPlatformArtifactId: String by project
val quarkusPlatformVersion: String by project

dependencies {
    implementation("io.quarkus:quarkus-smallrye-health")
    implementation("io.quarkus:quarkus-oidc")
    implementation("io.quarkiverse.loggingsentry:quarkus-logging-sentry:2.0.5")
    implementation(enforcedPlatform("${quarkusPlatformGroupId}:${quarkusPlatformArtifactId}:${quarkusPlatformVersion}"))
    implementation("io.quarkus:quarkus-container-image-docker")
    implementation(kotlin("reflect"))
    implementation("io.quarkus:quarkus-resteasy-reactive-jackson")
    implementation("com.fasterxml.jackson.dataformat:jackson-dataformat-xml")
    implementation("com.fasterxml.jackson.dataformat:jackson-dataformat-toml")
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
    implementation("io.quarkus:quarkus-rest-client-reactive")
    implementation("io.quarkus:quarkus-rest-client-reactive-jackson")
    implementation("io.quarkus:quarkus-scheduler")
    implementation("io.quarkus:quarkus-quartz")
    implementation("io.quarkus:quarkus-jdbc-postgresql")
    implementation("io.quarkus:quarkus-micrometer")
    implementation("io.quarkus:quarkus-grpc")
    implementation("io.quarkus:quarkus-websockets-client")
    implementation("io.quarkus:quarkus-smallrye-openapi")
    implementation("io.quarkus:quarkus-websockets")
    implementation("io.quarkus:quarkus-opentelemetry")
    implementation("io.opentelemetry.instrumentation:opentelemetry-jdbc")
    implementation("io.opentelemetry.instrumentation:opentelemetry-okhttp-3.0")
    implementation("io.opentelemetry:opentelemetry-extension-kotlin")
    implementation("io.quarkus:quarkus-kotlin")
    implementation("io.quarkus:quarkus-hibernate-orm-panache-kotlin")
    implementation("io.hypersistence:hypersistence-utils-hibernate-63:3.7.3")
    implementation("io.quarkus:quarkus-flyway")
    implementation("org.jetbrains.kotlin:kotlin-stdlib-jdk8")
    implementation("io.ktor:ktor-client-okhttp:2.3.2")
    // Event streams
    implementation("io.smallrye.reactive:mutiny-kotlin:2.0.0")
    implementation("io.quarkus:quarkus-arc")
    implementation("io.quarkus:quarkus-hibernate-orm")
    implementation("io.quarkus:quarkus-hibernate-validator")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation(files("libs/jgit.jar", "libs/libjgit-servlet.jar"))
    implementation("io.quarkus:quarkus-undertow")
    implementation("commons-io:commons-io")
    implementation("io.quarkus:quarkus-keycloak-admin-client-reactive")
    implementation("io.jsonwebtoken:jjwt-api:0.12.6")
    implementation("io.jsonwebtoken:jjwt-impl:0.12.6")
    implementation("io.jsonwebtoken:jjwt-jackson:0.12.6")
    implementation("com.goterl:lazysodium-java:5.1.4")

    testImplementation("io.quarkus:quarkus-junit5")
    testImplementation("io.quarkus:quarkus-junit5-component")
    testImplementation("io.quarkus:quarkus-junit5-mockito")
    testImplementation("io.rest-assured:rest-assured")
}

group = "org.bismuth"
version = "1.0.0-SNAPSHOT"

java {
    sourceCompatibility = JavaVersion.VERSION_21
    targetCompatibility = JavaVersion.VERSION_21
}

tasks.withType<Test> {
    systemProperty(
        "java.util.logging.manager",
        "org.jboss.logmanager.LogManager"
    )
}
allOpen {
    annotation("jakarta.ws.rs.Path")
    annotation("jakarta.enterprise.context.ApplicationScoped")
    annotation("jakarta.persistence.Entity")
    annotation("io.quarkus.test.junit.QuarkusTest")
}

tasks.compileKotlin {
    dependsOn(tasks.compileQuarkusGeneratedSourcesJava)

    kotlinOptions.jvmTarget = JavaVersion.VERSION_21.toString()
    kotlinOptions.javaParameters = true
    kotlinOptions.freeCompilerArgs += "-Xjvm-default=all"
}

tasks.quarkusDev {
    compilerOptions {
        compiler("kotlin").args(
            listOf("-Xjvm-default=all")
        )
    }
}
