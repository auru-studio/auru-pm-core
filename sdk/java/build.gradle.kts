plugins {
    `java-library`
    `maven-publish`
    signing
}

group = "studio.auru"
version = "0.1.0"

description =
    "Client for auru-pm-v1 project-management providers. Connects to any endpoint you give it."

java {
    // Baseline 17: the widest deployed LTS, and nothing here needs newer than
    // 11. Built and tested on 21, with `--release` so the bytecode and the API
    // surface both stay 17-compatible rather than only the bytecode.
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
    withJavadocJar()
}

// Deliberately no runtime dependencies.
//
// A client library that drags in a JSON stack causes version conflicts in
// somebody else's application, and — more importantly — canonical encoding
// needs exact control over JSON output. A third-party serializer's formatting
// choices would become our bug, and the bug would surface as a provider
// rejecting a commit. The same reasoning that made RFC 8785 the right rule
// makes owning the writer the right call.
dependencies {
    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

repositories {
    mavenCentral()
}

// Records document themselves in their class comment; demanding an @param for
// every component would mean repeating the component name back at the reader.
// Everything else doclint checks stays on.
tasks.withType<Javadoc>().configureEach {
    (options as StandardJavadocDocletOptions).addStringOption("Xdoclint:all,-missing", "-quiet")
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("failed")
        showStackTraces = true
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
    // A green build says nothing about how much ran; print the count so a
    // suite that silently stops matching tests is visible.
    addTestListener(
        object : TestListener {
            override fun beforeSuite(suite: TestDescriptor) {}

            override fun beforeTest(test: TestDescriptor) {}

            override fun afterTest(test: TestDescriptor, result: TestResult) {}

            override fun afterSuite(suite: TestDescriptor, result: TestResult) {
                if (suite.parent == null) {
                    logger.lifecycle(
                        "tests: ${result.testCount} run, ${result.failedTestCount} failed, " +
                            "${result.skippedTestCount} skipped",
                    )
                }
            }
        },
    )
    // The conformance vectors and the reference server both live in the repo.
    systemProperty("auru.repoRoot", rootDir.resolve("../..").normalize().absolutePath)
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            artifactId = "auru-pm"
            from(components["java"])
            // Maven Central rejects a POM missing any of these, and it does so
            // only at upload — long after the version has been tagged.
            pom {
                name = "auru-pm"
                description = project.description
                url = "https://github.com/auru-studio/auru-pm-core"
                licenses {
                    license {
                        name = "MIT License"
                        url = "https://opensource.org/licenses/MIT"
                        distribution = "repo"
                    }
                    license {
                        name = "Apache License, Version 2.0"
                        url = "https://www.apache.org/licenses/LICENSE-2.0"
                        distribution = "repo"
                    }
                }
                developers {
                    developer {
                        id = "auru-studio"
                        name = "Auru"
                        url = "https://github.com/auru-studio"
                    }
                }
                scm {
                    url = "https://github.com/auru-studio/auru-pm-core"
                    connection = "scm:git:https://github.com/auru-studio/auru-pm-core.git"
                    developerConnection = "scm:git:ssh://git@github.com/auru-studio/auru-pm-core.git"
                }
            }
        }
    }

    repositories {
        // A local staging repository. The release workflow zips this and hands
        // it to the Central Portal, which keeps credentials out of the build
        // and makes exactly what would be published inspectable beforehand.
        maven {
            name = "staging"
            url = uri(layout.buildDirectory.dir("staging-repository"))
        }
    }
}

signing {
    // Central requires a detached signature per artifact. Keys come from the
    // environment so a local build needs none and never half-signs.
    val signingKey: String? = System.getenv("SIGNING_KEY")
    val signingPassword: String? = System.getenv("SIGNING_PASSWORD")
    isRequired = signingKey != null
    if (signingKey != null) {
        useInMemoryPgpKeys(signingKey, signingPassword)
        sign(publishing.publications["maven"])
    }
}

// Fails the build rather than the upload when something Central insists on is
// missing, which is the difference between a five-second fix and a burnt tag.
tasks.register("verifyPublishable") {
    group = "verification"
    description = "Check the artifacts and POM Maven Central requires."
    dependsOn("publishMavenPublicationToStagingRepository")
    doLast {
        val staging = layout.buildDirectory.dir("staging-repository").get().asFile
        val expected = listOf(".jar", "-sources.jar", "-javadoc.jar", ".pom")
        val produced = staging.walkTopDown().filter { it.isFile }.map { it.name }.toList()
        for (suffix in expected) {
            require(produced.any { it.endsWith(suffix) }) {
                "no artifact ending in $suffix; Maven Central requires all of $expected"
            }
        }
        val pom = staging.walkTopDown().first { it.name.endsWith(".pom") }.readText()
        for (element in listOf("<name>", "<description>", "<url>", "<licenses>", "<developers>", "<scm>")) {
            require(pom.contains(element)) { "the POM is missing $element, which Maven Central requires" }
        }
        logger.lifecycle("publishable: ${produced.size} artifacts staged in ${staging.name}")
    }
}
