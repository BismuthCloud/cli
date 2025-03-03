package org.bismuth

import io.quarkus.hibernate.orm.panache.kotlin.PanacheCompanion
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntityBase
import jakarta.persistence.*
import org.jboss.logging.Logger

interface BismuthPanacheCompanionInterface<Entity : PanacheEntityBase> : PanacheCompanion<Entity> {
    fun <U : Any> findBy(property: String, value: U): Entity? {
        val LOG: Logger = Logger.getLogger(javaClass)

        return try {
            val _class = this::class.java.enclosingClass.simpleName
            val query = "FROM $_class E WHERE E.$property = ?1"

            find(query, value).firstResult()
        } catch (e: NoResultException) {
            null
        }
    }
}
