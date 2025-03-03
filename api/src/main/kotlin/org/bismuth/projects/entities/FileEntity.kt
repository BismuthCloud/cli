package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonBackReference
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime

@Entity
@Table(name = "files")
class FileEntity : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<FileEntity> {

    }

    enum class Type {
        MEDIA,
        CODE
    }

    @Enumerated(EnumType.STRING)
    lateinit var type: Type

    lateinit var hash: String

    lateinit var name: String
    lateinit var pathInProject: String

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @ManyToOne(fetch = FetchType.LAZY)
    @JoinColumn(name = "featureId")
    @JsonBackReference
    lateinit var feature: FeatureEntity
}