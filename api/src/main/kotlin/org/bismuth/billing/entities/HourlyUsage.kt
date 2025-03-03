package org.bismuth.billing.entities

import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.bismuth.auth.entities.OrganizationEntity
import org.bismuth.projects.entities.FeatureEntity
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime
import java.time.OffsetDateTime


@Entity
@Table(name = "hourly_usage")
class HourlyUsage() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<HourlyUsage> {

    }

    @JoinColumn(name = "featureId")
    @ManyToOne(fetch = FetchType.LAZY)
    lateinit var feature: FeatureEntity

    @JoinColumn(name = "orgId")
    @ManyToOne(fetch = FetchType.LAZY)
    lateinit var organization: OrganizationEntity

    // Day + hour this record is for
    lateinit var time: OffsetDateTime

    lateinit var item: String

    var usage: Long? = null

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null
}